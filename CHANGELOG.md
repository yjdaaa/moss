# Changelog

代码修复与功能变更记录，格式参考 Keep a Changelog；条目要求见 [`AGENTS.md`](AGENTS.md)“变更记录”。

## [Unreleased]

### 新增本地合成模型 Qwen3-TTS 1.7B 并按说话指令调语速，不支持调语速的模型不再显示语速框（2026-11-25）

- **现象**：Qwen3-TTS 0.6B 不支持调语速，语速框却仍显示（上一版是置灰）；想要 Qwen3 的音色又想调语速没有办法。
- **根因与修复**：`speech.cpp v0.10.1` 的 Qwen3-TTS 拒收 `speed`，只有 1.7B 接受说话指令 `instructions`。`CATALOG` 新增第 29 项 `qwen3_tts_1_7b`（Q8_0 2.29 GB，Apache-2.0，与 0.6B 共用运行时与 9 个音色），`speech_tts_instructions` 把语速分五档换成指令（实测约 0.54x～1.19x，加速最多约 1.2 倍）；模型状态新增 `speed_adjustable`，为 `false` 时前端不显示语速框。默认合成模型不变。
- **验证**：真实 1.7B 模型经 `speech` 实测五档指令时长（同句 3 个种子），新增 `worker_sends_speaking_instructions_for_qwen3_tts_1_7b`、`qwen3_tts_1_7b_maps_speed_to_coarse_instructions_and_0_6b_never_gets_any` 与目录、前端分组 / 删除提示单测；`verify.mjs --finish` 通过。未在真实小智设备上收听（`unverified`）。
- **关联**：`crates/moss-voice/src/local_models.rs`（`qwen3_tts_1_7b`、`tts_speed_adjustable`、`speech_runtime_root`）、`crates/moss-voice/src/tts_voices.rs`（`speech_tts_instructions`）、`crates/moss-voice-providers/src/speech_cpp.rs`、`crates/moss-voice-host/src/builtin/conversation.rs`、`frontend/src/components/voice-builtin/LocalTtsSettings.jsx`、内置语音技术书 n126。

### 修复千问 / 百炼语音合成与声音克隆的 SSRF 防护、模型音色绑定及跨区复用边界（2026-11-25）

- **现象**：非流式回退下载 `output.audio.url` 未固定 DNS 解析且回环网关未拦内网 HTTPS，插话打断时流式下载未立即终止；克隆音色下拉点选未联动切换绑定的 `target_model`、切换模型残留不兼容音色、跨分区复用会双向误推非千问或无语音端点的地址、保存时可能跨账号串入克隆音色缓存，且 24-bit / 扩展头 WAV 录音在本地被误拒。
- **根因与修复**：`download_fallback_audio` 异步解析并校验全部 IP 非私网/回环后用 `.no_proxy().redirect(Policy::none()).resolve_to_addrs(...)` 固定地址在剩余总超时内下载，`DashscopeTts::speak` 监听接收端关闭即刻断流并按响应头区分 SSE 与裸 WAV/JSON；`enroll_voice` 轮询缺省状态改为 `DEPLOYING` 并跑满 60 秒总预算，本地 WAV 头解析兼容 24/32-bit PCM 与 `WAVE_FORMAT_EXTENSIBLE`；后端五处入口统一拦截纯对话网关、`token-plan.*` 与 `sk-sp-` 密钥，前后端校验克隆音色模型前缀与 `_v3.1` 系统音色范围、点选克隆音色自动同步 `model`、收紧跨区复用并按 `savedFromVoicesKey` 隔离缓存，同步补齐架构与规范文档。
- **验证**：`dashscope` 新增 24-bit/扩展头 WAV、流式打断即停、轮询缺省状态等待与回环/DNS 内网拦截测试，`moss-voice` 新增纯对话网关与异模型音色拦截测试，`voiceState.test.mjs` 与 `voice-center.browser.mjs` 新增跨服务商复用隔离及点选克隆音色联动模型断言；`verify.mjs --finish` 通过。未用真实千问云端账号与小智真机实测（`unverified`）。
- **关联**：`crates/moss-voice-providers/src/dashscope.rs`、`crates/moss-voice/src/models.rs`、`crates/moss-voice-host/src/builtin/models.rs`、`frontend/src/voiceState/sections.js`、`frontend/src/components/voice-builtin/BuiltinModelGroup.jsx`、内置语音技术书 n125。

### 本地合成音色改为按名称下拉，Qwen3-TTS 语速框置灰，新增按当前音色和语速试一句（2026-11-25）

- **现象**：“5. 语音合成模型包”下的音色是编号输入框，Qwen3-TTS 的标签塞进 9 个音色说明、把两栏撑歪；Qwen3-TTS 不支持调语速但语速框照样能改；想听新语速只能先保存再跑连贯试跑，语言包与试听弹窗的试听固定 1.0x。
- **根因与修复**：音色与语速抽成 `LocalTtsSettings`，音色读该模型音色表做按语种分组的名称下拉（`localTtsSpeakerGroups`，读不到退回编号框），`localTtsSpeedFixed` 为真时语速框置灰并写明原因；新增“按当前音色和语速试一句”，`local_tts_preview` 接受可选 `speed`（0.5～2.0，越界 400），非 1.0 时当场合成、不读也不写 1.0x 试听缓存，Qwen3-TTS 固定 1.0。保存口径不变：对话、连贯试跑与弹窗“当前启用”仍只认已保存的设置。
- **验证**：`local_runtime_tests` 补非 1.0x 不拿缓存冒充、越界语速拒绝、不覆盖缓存；`voiceState.test.mjs` 新增音色分组与固定语速判断；`voice-center.browser.mjs` 覆盖下拉分组、不悄悄换音色与“试一句”带未保存语速；`verify.mjs --finish` 通过。未在真实设备上收听（`unverified`）。
- **关联**：`frontend/src/components/voice-builtin/LocalTtsSettings.jsx`、`frontend/src/voiceState/localModels.js`、`frontend/src/voiceApi.js`（`localTtsPreview`）、`crates/moss-voice-host/src/builtin/tts_preview.rs`（`local_tts_preview`）、内置语音技术书 n124。

### 语音合成支持千问 / 百炼原生流式接口（`Qwen-Audio-3.1-TTS-Flash`）与本机录音一键克隆音色（2026-11-25）

- **现象**：千问 AI 平台（`maas.qianwenaiapi.com`）与阿里云百炼不提供 OpenAI `/v1/audio/speech` 接口，填进原有语音合成会报 404，也无法在 MOSS 内上传参考录音克隆专属音色。
- **根因与修复**：新增 `ProviderKind::Dashscope` 与 `DashscopeTts`，支持将根域名或大模型 `/compatible-mode/v1` 自动转换到 `/api/v1/services/audio/tts/SpeechSynthesizer`（SSE 流式 24 kHz PCM，`streaming_tts = true`）与 `/customization`（`voice-enrollment`），拦截不支持语音的 `token-plan.*` 地址与 `sk-sp-` 密钥，并支持复用同一 HTTPS 主机的大模型密钥；「语音合成」卡片新增“千问 / 百炼（流式合成 · 支持声音克隆）”与“上传录音克隆音色”（本机 WAV/MP3/M4A 录音校验后创建复刻音色、轮询就绪、自动回填 `voice_id` 并生成试听），同时读取账号已有克隆音色列表，默认语音合成配置不变。
- **验证**：新增 `DashscopeTts` 流式合成 / 音色克隆与列表回环测试 3 项、`moss-voice` 地址转换与同源密钥测试、前端单测与 `voice-center` 浏览器测试克隆音色交互断言；`verify.mjs --finish` 通过。未用真实千问密钥与小智硬件实测（`unverified`）。
- **关联**：`crates/moss-voice/src/models.rs`（`ProviderKind::Dashscope`）、`crates/moss-voice-providers/src/dashscope.rs`、`crates/moss-voice-host/src/builtin/models.rs`（`tts_voice_clone`）、`frontend/src/components/voice-builtin/BuiltinModelGroup.jsx`、内置语音技术书 n123。

### 新增本地合成模型“MOSS 原声（流浪地球，VITS）”，断网也能用电影 MOSS 的声音播报（2026-11-25）

- **现象**：想让小智用电影 MOSS 的声音说话，之前只能开 Colab T4 联动的 550W 克隆声（要联网、要 Colab），本地合成模型里没有 MOSS 声线。
- **根因与修复**：`local_models::CATALOG` 新增第 28 项 `moss_vits`（`Engine::Vits`），从 `OpenMOSS/moss-vits-onnx-model` 下载 `moss.onnx`、从 sherpa-onnx 官方中文 VITS 下载同一套注音词表与数字读法规则，两个仓库钉具体提交并逐个校验 SHA-256（新增 `installed_rel_path` 让 `Archive::Directory` 跨仓库装进同一目录）；`moss-voice-local` 新增 `vits_zh`，首次加载在模型目录派生带 ONNX 元数据与词界 `▁` 的文件、合成前整理百分号 / 单位 / MOSS / 大写缩写。辅助程序协议 8 → 9（新增 `TtsEngine::Vits`，须与桌面程序一起升级）；默认合成模型不变；权重仅限非商用，只在用户点下载时从原地址拉取。
- **验证**：新增 `hf_repo_qualified_paths_install_flat_into_model_dir`、`moss_vits_spec_pulls_two_pinned_hf_repos_with_mirror_fallback`、`vits_zh` 4 项与 `vits_tts_config_uses_derived_files_and_plain_named_rule_fsts`；用真实模型经 `moss-voice-local` 协议加载 0.3 秒、整段与流式合成通过（1 秒语音约 0.1 秒），派生文件与关掉噪声时逐采样一致的对照版本逐字节相同；`verify.mjs --finish` 通过。未在真实小智设备上收听（`unverified`）。
- **关联**：`crates/moss-voice/src/local_models.rs`、`crates/moss-voice/src/tts_voices.rs`、`crates/moss-voice-local-proto/src/lib.rs`、`crates/moss-voice-host/src/builtin/conversation.rs`（`helper_tts_engine`）、`apps/moss-voice-local/src/vits_zh.rs`、`apps/moss-voice-local/src/main.rs`（`tts_config`、`rule_fsts`）、内置语音技术书 n122。

### 本地合成音色与语速挪进“语音合成模型包”，选完模型就近调整（2026-11-25）

- **现象**：本地合成的音色编号与语速框放在“本地模型与音色”卡片最底部，和合成模型表隔着视觉理解、声纹等分组，用户在“5. 语音合成模型包”里找不到语速可调。
- **根因与修复**：`LocalModelsGroup` 把音色编号 / 试听入口与语速框移到语音合成模型表下方（`#voice-local-tts-settings`），下载地址仍留在卡片底部；字段、校验、保存方式与 Qwen3-TTS 固定 1.0x 的提示都不变。
- **验证**：`voice-center` 浏览器测试新增语速框、音色入口位于语音合成模型包内的断言；`verify.mjs --finish` 通过（前端单测、浏览器测试 18 项）。
- **关联**：`frontend/src/components/voice-builtin/LocalModelsGroup.jsx`（`renderOther`）、`docs/developer/frontend-behavior.md`。

### 本地模型按语音链路顺序排列，顶部选了未下载的模型直接跳到它的“下载”按钮（2026-11-25）

- **现象**：语音中心「模型」标签的“本地模型（断网可用）”里，分组顺序是判停 → 识别 → 合成 → 视觉 → Jev，和顶部“当前语音链路”1～5 栏对不上；顶部选了未下载的本地模型只展开整个分组，用户要自己往下找那一行才能点下载。
- **根因与修复**：`LocalModelsGroup` 改按链路执行顺序排列并加同样的序号（1 判停 → 2 识别 → 3 Jev → 4 大模型 Colab 联动 → 5 合成，视觉、声纹放最后），模型行带 `voice-local-row-<id>`；新增 `focusLocalModel`，顶部选了未下载的判停 / 识别 / Jev / 合成都滚到该行、聚焦“下载”按钮并高亮约 2.4 秒（收起区先展开；分组原本收起时等展开渲染完再聚焦，用户已移走焦点则不抢）。只改前端展示，设置与下载行为不变。
- **验证**：`voice-center` 浏览器测试新增跳转聚焦“下载”按钮、行高亮与分组顺序断言（首轮暴露分组收起时聚焦失败，已修）；`verify.mjs --finish` 通过（前端单测 391 项、浏览器测试 18 项）。
- **关联**：`frontend/src/components/voice-builtin/LocalModelsGroup.jsx`、`shared.js`（`focusLocalModel`）、`BuiltinModelGroups.jsx`（`handleSelectSource`）、`styles/09-voice-center.css`、内置语音技术书 n121。

### 本地语音合成输出先隔直，消除 Kokoro 随音量起伏的直流偏移（2026-11-25）

- **现象**：Windows「连贯试跑」导出的 Kokoro 合成音频（种子句与第 5 棒回复）整体偏向正半轴：每 100 ms 均值达有效值的 0.4～0.6 倍，0～20 Hz 占全段约三成能量，正峰 0.63、负峰仅 0.29～0.44，扬声器每个分句开头会被推一下，负向余量浪费一半，软限幅只压正峰。
- **根因与修复**：声码器输出自带直流，`moss-voice-local` 转 16-bit 前只做软限幅、没有隔直。新增 `remove_dc`（一阶 25 Hz 隔直高通，80 Hz 以上衰减 <0.5 dB），整段合成与流式子分段都经 `tts_pcm` 先隔直再 `to_i16`，对所有 sherpa-onnx 本地合成引擎生效；Qwen3-TTS（speech.cpp）与云端合成不经此路径。
- **验证**：新增 `tts_dc_block_removes_envelope_offset_keeps_voice_band_and_settles_in_silence`；同一滤波套用到 Windows 导出音频后 0～20 Hz 能量从 31% / 27% 降到 0.4% / 0.3%，人声均值归零。本机无 Kokoro 模型，实际合成与听感未验证（`unverified`）。
- **关联**：`apps/moss-voice-local/src/main.rs`（`remove_dc`、`tts_pcm`、`TTS_DC_BLOCK_HZ`）、内置语音技术书 n120。

### 修复本地 Kokoro 语音合成一字一顿与缺变调问题，消除分句二次切碎并拉开逗句停顿层次（2026-11-25）

- **现象**：使用本地 Kokoro 合成语音（真机与「连贯试跑 / 录真话测」）时，“三毫米 / 椴木 / 刻字 / 确认 / 对焦 / 两遍”等常用词组被逐字拆散念出一字一顿感，“切不透 / 一般 / 你好”缺少普通话连读变调，且一句话在逗号处被二次切碎并把句号与逗号停顿压成同一时长，缺少真人说话的连贯起伏。
- **根因与修复**：`moss-voice-local` 新增 `kokoro_prosody` 模块，针对 `sherpa-onnx 1.13.8` 忽略 `jieba` 词典、缺失中文词界记号与 v1.0 缺变调的问题，不改模型原目录派生缓存增强词表（v1.0 映射单码点非空白词界 `▁ -> space_id`、v1.1 复用 `/`，复合词拼装优先继承原词表多音子词并融合工坊高频词与 `jieba.dict.utf8` 常用词，完成“一 / 不 / 三声”变调，`tts_voices` 试听缓存版本升至 `v2`）；`Segmenter`（含流式 SSE 逐字到达）与 `synthesis_segments_with` 对句末 4～8 字短收尾不再从逗号硬拆，`Pacing::first_max_chars` 在有待播音频垫底且实时率允许时按算力放宽至 18～28 字整句合成（子分段换气从 80ms 调至 130ms）；`moss-voice-server` 新增 `clause_silence_bounds_ms` 将逗号（140ms）、分号（200ms）与句号（250ms）停顿分层（真机与 `chain_test` 同步生效），并优化 `RISK_NOTES_RULE` 避免拆出孤立四字短句。
- **验证**：新增 `kokoro_prosody` 词界、多音子词继承、普通话变调与并发缓存复用单测，补充 `Segmenter` 流式短尾合并、`synthesis_segments_with` 整句直通、`Pacing` 算力分档放宽及真机流式逗句分层停顿单测；`verify.mjs --finish`（含 `moss-voice-local` 独立工作区检查与测试）通过并快更本机桌面程序。真机与更新后本地实际听感未验证（`unverified`）。
- **关联**：`apps/moss-voice-local/src/kokoro_prosody.rs`、`apps/moss-voice-local/src/main.rs`、`crates/moss-voice-local-proto/src/lib.rs`、`crates/moss-voice-providers/src/local.rs`、`crates/moss-voice-providers/src/text.rs`、`crates/moss-voice-server/src/audio/mod.rs`、`crates/moss-voice-server/src/conversation.rs`、`crates/moss-voice-host/src/builtin/chain_test.rs`、内置语音技术书 n119。

### 补齐五步连贯试跑 Jev 快路径与完整工具组评估，修复切换云端地址时的模型列表串缓存（2026-11-25）

- **现象**：「连贯试跑 / 录真话测」在开启 Jev 时对“退下 / 再见”退出词与“机器状态”等本地只读快答未像真机一样 0ms 跳过 Jev，Jev 工具剪枝评估只带激光与外部工具而漏掉角色/信息/家居/音乐 4 个工具组，且仅勾选尾音续听与只读快答遇到含逗号句子时会因 `evaluate_turn` 本地直通而漏调 `check_unfinished_hold`；语音中心切换云端地址后未重新拉取列表即保存时，会把旧端点的模型列表写进新端点缓存。
- **根因与修复**：`BuiltinVoice::chain_test` 同步豁免默认退出短令（`is_default_exit_phrase`）与本地只读快答（`fast::read_intent`），抽取出与真机一致的 `jev_tool_specs` 六大工具组规格清单，并在 `decision.local_bypass && jev_options.unfinished_hold` 时继续执行 `check_unfinished_hold` 实测 Jev 连通性；`BuiltinModelGroup.jsx` 的 `save()` 仅在 `prev.key === savedFromListKey` 时写入新缓存键并保留 `suggestedUrl`，切换端点未重拉时清空旧列表并递增请求序号。
- **验证**：扩展 `chain_test_runs_five_steps_end_to_end_and_reports_exact_failing_stage`（覆盖退出词与只读快答免 Jev 直通及完整工具组断言）与 `voice-center` 浏览器交互测试（覆盖跨端点保存不串模型缓存）；`verify.mjs --finish` 通过并快更本机桌面程序。未连接真机与真实云端实测（`unverified`）。
- **关联**：`crates/moss-voice-host/src/builtin/chain_test.rs`、`crates/moss-voice-host/src/builtin/conversation.rs`、`crates/moss-voice-server/src/conversation.rs`、`crates/moss-voice-server/src/music/tools.rs`、`frontend/src/components/voice-builtin/BuiltinModelGroup.jsx`。

### 消除语音对话客服腔与碎顿，五步连贯试跑对齐真机全链路处理（2026-11-24）

- **现象**：运行语音中心「连贯试跑」时，测试句“你好，汇报一下当前待命状态。”被识别成“一号。”，大模型回复“😊 好，我在，请说指令。”并带出屏幕专用表情，听感机械生硬；真机系统提示词也引导用“好，”“嗯，”拆成两三字碎顿，容易诱发对讲机口令腔。
- **根因与修复**：`moss-voice-server` 更新 `PLAIN_HEAD` 与 `IMMUTABLE_SAFETY_RULES`，明确禁止“请说指令 / 随时待命 / 有什么可以帮您”等客服套话，将短句改为一口气自然说完、长句在 6～12 字处停顿断开（匹配 `Segmenter` 6 字首逗阈值，`moss_station` 角色与全部安全确认铁律不变）；`BuiltinVoice::chain_test` 对齐真机会话全套处理：换用日常工坊测试句，首尾静音扩至 500ms/800ms，判停前置缓冲从 160ms 对齐真机 `1000ms` 并合并句中逗号气口多段人声，接入已开启的降噪、命令词抗噪融合与 Smart Turn 语义判停，识别优先走 `StreamingAsr` 流式会话识别（失败回退整段），对话走 `build_turn_system_prompt` + `ThinkFilter` + `Segmenter` 切句并剥除仅小智副屏使用的句首表情，合成按分句逐句调用 `Tts::synthesize_stream` 与句界淡入淡出。
- **验证**：新增 `n118_natural_workshop_tone_forbids_robotic_cliches_while_keeping_safety_gates`，扩展 `chain_test_runs_five_steps_end_to_end_and_reports_exact_failing_stage` 覆盖逗号停顿双段人声合并、`<think>` 与句首表情剥离及 `Segmenter` 分句流式合成；`verify.mjs --finish` 通过并快更本机桌面程序。小智真机对话自然度未实机验证（`unverified`）。
- **关联**：`crates/moss-voice-server/src/conversation.rs`、`crates/moss-voice-host/src/builtin/chain_test.rs`、`crates/moss-voice-host/src/builtin/conversation.rs`、内置语音技术书 n118。

### 读模型列表不再边打字边请求，裸域名提示改用 /v1，多模态模型回到语音分区（2026-11-24）

- **现象**：上一条改动后，在地址或密钥框打字停顿 0.35 秒就读一次模型列表，密钥没输完先报一次鉴权失败；只填裸域名时 `/models` 404 后改读 `/v1/models` 能列出模型，但对话不补 `/v1` 会失败；能听音频的多模态对话模型只剩在“大模型”列表，原先在语音识别里选过的要改成手填。
- **根因与修复**：自动读取改为停手 1 秒（`MODELS_AUTO_FETCH_DEBOUNCE_MS`）；`list_models` 走 `/v1` 回退时在结果里带 `suggested_url`，前端显示 `#voice-builtin-use-v1-*`“改用 …/v1”一键改地址；`extract_item_sections` 让“文本进文本出”的模型归大模型，同时“能听音频出文本”的也留在语音识别、“能出音频”的也留在语音合成（纯识别 / 纯合成模型不进大模型）。
- **验证**：`qwen_maas_wrapped_urls_plan_fallbacks_pagination_and_auth_hints_work` 补 `suggested_url` 与多模态分区断言，`voice-center` 浏览器测试补裸域名一键改用 `/v1`；`verify.mjs --finish` 通过（全工作区 clippy / Rust 测试、前端单测 391/391 与构建、浏览器测试 18/18、本机桌面程序快更与签名校验）。未用真实服务商实测（`unverified`）。
- **关联**：`crates/moss-voice/src/models.rs`（`ModelsListResult::suggested_url`、`list_models`、`extract_item_sections`）、`frontend/src/components/voice-builtin/BuiltinModelGroup.jsx`。

### 修复语音中心千问开放平台与套餐端点看不见模型、折行地址静默失效与测试慢思考问题（2026-11-24）

- **现象**：语音中心「大模型」填入千问开放平台 `maas.qianwenaiapi.com`（含多行复制带入换行空格、只填裸域名，或将 `sk-sp-` Token Plan 套餐密钥填进按量地址）时，模型下拉框不显示可选模型，自动拉取报错在未展开下拉时静默隐藏；拉回多模态模型后支持音频理解的对话模型被误筛出大模型列表，且千问新域名未附带关闭思考参数导致测试耗时偏长。
- **根因与修复**：前后端统一清理 URL 折行空白与大写协议头并幂等补齐千问 / 百炼 / Qwen Cloud 裸域名 `/compatible-mode/v1`；对齐 `magpie` 预置千问常用模型及 Token Plan / 火山方舟套餐模型（未填密钥或 `/models` 返回 404 时直接可选，套餐密钥与按量地址混填时给出直白提示与一键切换按钮，绝不回显密钥）；`BuiltinModelGroup` 在编辑态防抖自动读取列表、在模型栏下方直显失败原因并支持跨出容器失焦收起；`moss-voice::models` 补齐 `User-Agent`、`Accept: application/json`、8 MB 上限、剩余超时内的 `has_more` 分页与裸地址 `/v1/models` 回退，修正 `modalities` 将支持音频输入的多模态对话模型保留在 `Llm`；`openai::thinking_off_params` 补齐 `qianwenaiapi.com` / `qwencloudapi.com` / `maas.aliyuncs.com`。
- **验证**：`verify.mjs --finish` 通过：全工作区 clippy / Rust 测试（新增 `qwen_maas_wrapped_urls_plan_fallbacks_pagination_and_auth_hints_work` 与 `openai::tests::urls_are_normalized` 断言）、前端 ESLint / 单测 391/391 / 构建、浏览器交互测试 18/18（`voice-center` 新增折行地址清理、预置模型可见与 `sk-sp-` 一键切换断言）及本机桌面程序快更与签名校验全部通过；未用真实云端密钥在线实测（`unverified`）。
- **关联**：`crates/moss-voice/src/models.rs`、`crates/moss-voice-providers/src/openai.rs`、`frontend/src/voiceState/sections.js`、`frontend/src/components/voice-builtin/BuiltinModelGroup.jsx`、`frontend/src/styles/09-voice-center.css`、`docs/developer/frontend-behavior.md`。

### 断网时也能念出报错原因，快速查询失败不再念工具原文（2026-11-24）

- **现象**：n116 改成按原因播报后，“连不上大模型，请检查网络”这类话术没有预合成，断网时云端合成也连不上，只能在屏幕上显示文字（原先的笼统话术是预合成的）；本地只读快答（“机器状态”等）失败时把工具原文拼进“查询失败：…”，可能念出一长串、设备地址或英文报错。
- **根因与修复**：`Conversation::fixed_phrases` 在默认话术下改为预合成 4 句断网 / 超时原因（`LLM_TIMEOUT_REPLY` 等），不再预合成已不会念的旧话术，自定义 `error_reply` 照旧；新增 `read_failure_speech`，快答失败只说“查询超时，请稍后再问”“激光机没连上，请检查连接”“激光机还没就绪”等闭集短句，原文仍记在工具调试事件里；删掉大模型出错分支里不会执行到的等待。
- **验证**：新增 `network_error_replies_are_prewarmed_and_read_failures_never_speak_raw_tool_text`，`fixed_routes_skip_llm_keep_full_progress_and_report_failure` 增加查状态失败用例；`verify.mjs --finish` 通过（全工作区 clippy / Rust 测试，`moss-voice-server` 409 项通过、8 项忽略，本机桌面程序快更与签名校验）。断网实机播报未验证（`unverified`）。
- **关联**：`crates/moss-voice-server/src/conversation.rs`（`fixed_phrases`、`read_failure_speech`、`read_reply`）、内置语音技术书 n117。

### 语音对话全链路出错简短直报原因，修复连说退出与本地 Jev 双重超时阻塞（2026-11-24）

- **现象**：识别、大模型或工具超限出错时小智只会笼统说“抱歉，我这边出了点问题”，且说了开场白后第二轮工具汇总大模型报错、中途断流、后台查询回报出错、点歌混调及本地音乐缺文件/解码出错时直接静默无声；说“退下退下”“停停”会漏出快路径落入 Jev 与大模型；本地单槽 OneJev 在说话期预热被静音判停中止重发时自撞，续听超时后同轮 `evaluate_turn` 再卡满 1500 ms。
- **根因与修复**：`conversation::spoken_error_reply` 在默认话术下按故障环节与 `DiagCategory` 映射为 6～15 字闭集静态中文短句（不含套话，绝不拼入密钥、地址或英文原文，自定义 `error_reply` 仍保留），并补齐开场白后/中途断流、后台查询播报、点歌混调及本地音乐启动/解码异常的语音播报；`is_exit` 与 `fast::is_stop` 支持两三遍连说且命中退出/急停/本地快答时跳过 Jev 等待；静音判停复用同句预热，`check_unfinished_hold` 超时后跳过同轮 `evaluate_turn`，回环 `127.0.0.1`/`localhost` 直连（`.no_proxy()` + 250 ms 连接超时），`jev_hold` 轨迹区分 `skipped` 与 `finished`，`completeness` 补无标点问句结尾。
- **验证**：`verify.mjs --finish` 通过：全工作区 clippy / Rust 测试（`moss-voice-server` 416/416 通过，新增 `spoken_errors_explain_category_in_plain_chinese_without_leaking_secrets`、`llm_failure_after_preamble_and_tool_call_still_speaks_concise_error`、`missing_music_file_after_spoken_preamble_still_speaks_reason_after_preamble`、`reduplicated_exit_skips_jev_hold_and_same_interim_prewarm_is_reused_without_duplicate_call` 等 6 项）、分层与规范检查及本机桌面程序快更通过；待 Windows 真机复测（`unverified`）。
- **关联**：`crates/moss-voice-server/src/{conversation,fast,jev,session}.rs`、`session/{lifecycle,listen,playback,trace,turn}.rs`、`docs/integration/moss-native-voice-spec.md`、内置语音技术书 n116。

### 修复语音模型列表样式冲突，打磨曜石下拉框、侧边栏过渡与工作台卡片布局（2026-11-24）

- **现象**：① 语音中心模型下拉列表与场景推荐套系重用 `.voice-combo-list` 导致内边距清零与 `220px` 限高互相串台；点 `<label>` 文字打不开曜石下拉浮层、长选项折成多行且向上弹出动画方向相反；② 侧边栏展开动画期间文字竖排挤成多行跳动、左侧激活竖条越界被裁切、窄屏折成多行且短页面纵向拉伸导航高度、隐藏顶栏胶囊后侧栏无待审批提示；③ 工作台底部控制台不可收起且标签无法键盘聚焦，加工内容四按钮宽窄状态高度不一，自由网格解锁态缺 `grab` 光标且右下浮窗展开易挡底部控件。
- **根因与修复**：① `ModelPairingGuide` 拆出 `.voice-preset-list` 解除样式冲突；`ObsidianSelectHost` 支持关联 `<label>` 点击开合、`Home`/`End` 跳首尾、窗口失焦收起、`data-upward` 向上动画与长选项单行省略（全文挂 `title`）；② `SideNav` 与 `06-shell.css` 补齐文字单行裁切、激活竖条内收防截断、`≤ 900px` 单行可横滑紧凑导航（配 `grid-template-rows: auto minmax(0, 1fr)`）及「语音中心」`.ob-nav-badge` 待审批计数；③ `ConsoleDrawer` 接入 `foldState` 收起记忆（`moss_ui_fold_wb-console-drawer`，默认展开，保留 G-code 虚拟列表偏移恢复）并将标签改为标准 `role="tablist"` / `<button role="tab">` / `role="tabpanel"`；`HomePage` 统一加工内容四按钮两元素居中结构与内联外跳图标，`WorkbenchCanvas` 按实际 `.ob-card-h` 判定拖拽并补齐 `grab` 光标与浮窗展开底部留白。后端接口与安全门禁不变。
- **验证**：`verify.mjs --finish` 通过：分层与仓库规范检查、脚本单测 75/75、前端 ESLint、前端单测 390/390（`uiPolishB`、`uiModalAndScrollFixes` 新增类名隔离、侧栏待审批徽标、`tablist` 与四按钮对齐断言）、生产构建、浏览器测试 18/18（`shell-chrome` 新增侧栏过渡不折行、左沿不裁切、`<label>` 唤起下拉、`Home`/`End`、底部抽屉折叠展开及 820px/600px 单行导航不重叠断言）及本机桌面程序快更全部通过；未连真实激光硬件实测（`unverified`）。
- **关联**：`frontend/src/components/{ObsidianSelectHost,SideNav,ConsoleDrawer,WorkbenchCanvas}.jsx`、`frontend/src/components/voice-builtin/ModelPairingGuide.jsx`、`frontend/src/pages/HomePage.jsx`、`frontend/src/styles/{02-controls,05-console,06-shell,07-pages,09-voice-center,10-component-inline,15-addenda-2026-11}.css`、`docs/developer/frontend-behavior.md`。

### Windows 上过程记录默认保存位置不再显示 `\\?\` 前缀，资源管理器能打开（2026-11-24）

- **现象**：Windows 上“完整过程记录”的默认保存位置显示成 `\\?\C:\Users\…\moss\logs\process-trace`，点“打开文件夹”时资源管理器不认这种写法，可能打不开或打开别处；自选目录不受影响。
- **根因与修复**：运行目录启动时经 `fs::canonicalize` 规范化，Windows 上会加 `\\?\` 逐字前缀，默认过程记录目录与运行目录下的其他路径都带上了它。新增 `laser_runtime::paths::without_verbatim_prefix`，运行目录规范化后去掉前缀（盘符路径与网络共享都处理，去掉后超过 260 字符的保留）；自选目录原有的去前缀改用同一函数；`exposed_path` 对老版本存下的带前缀路径也先去前缀。macOS / Linux 行为不变。
- **验证**：在只含本次改动的独立副本里跑 `verify.mjs --finish --no-update` 通过：全工作区 clippy / Rust 测试（新增 `verbatim_prefix_is_removed_from_drive_and_share_paths_only`，运行目录测试加“不带 `\\?\` 前缀”断言）、前端检查与浏览器测试、回环 E2E；本机没有 Windows 编译目标，只在 Windows 上生效的代码待 CI 的 Windows 构建与实机确认；没有快更本机程序。
- **关联**：`crates/laser-runtime/src/paths.rs`、`root.rs`（`RuntimeRoot::resolve`、`exposed_path`）、`process_trace/directory.rs`（`normalize_canonical_str`）。

### 统一材料编辑弹窗速度单位，折叠数字孪生 3D 摆位并收口弹窗与标签细节（2026-11-24）

- **现象**：① 添加/编辑材料弹窗用 `mm/s`（新建默认 `100` 折算达 `6000 mm/min`），与全站 `mm/min` 割裂且非 60 整倍数速度编辑往返有取整漂移；② 数字孪生首屏平铺两块仅改本页 3D 视角的摆位控件，挤出遥测与行程安全卡且易误导为真实对刀，另残留 `$CD`、裸 `workflow_id`、写死机型名与 `LINE`；③ 关闭窗口确认框残留“杀死进程”与错误的“右上角设置”指引，AI 预览窗与审批卡缺 `Esc` 收起，多处仍带重复英文小标并遗留两个零引用文件。
- **根因与修复**：① `MaterialModal` 速度统一改为 `mm/min`（新建默认 `1500 mm/min`，旁注 `mm/s`）并惰性初始化首帧表单，直写 `feed_rate` 零漂移；② `TwinPage` 将 3D 摆位收进默认折叠的 `#twin-scene-layout` 并注明不改实际刀路，新增 `formatTwinWorkflowLabel` 展示内容与材质摘要替代裸 `workflow_id`，清掉 `$CD`、写死机型名与 `LINE`；③ 修正 `CloseConfirmModal` 与 `SideNav` 文案并收拢静态内联样式；④ `ApprovalCenter` 与 `AiPreviewCenter` 补齐 `createPortal`、右上角关闭钮、`Escape`（经 `isTopmostModalBackdrop`）及遮罩收起为稍后处理；⑤ 清理顶栏、工具卡、实验室、提示框与发送覆盖层的重复英文小标，画板根改为 `<div>`，删除零引用的 `LogPanel.jsx` 与 `forgeDrawEntry.js`。后端契约与安全门禁不变。
- **验证**：`verify.mjs --finish` 通过：分层与仓库规范检查、脚本单测 75/75、前端 ESLint、前端单测 390/390（`targetedFixes`、`uiModalAndScrollFixes`、`uiPolishB` 补齐速度无损回显、Portal/`Esc` 与标签断言）、生产构建、浏览器测试 18/18（`twin-estop`、`ai-preview` 补折叠展开与栈顶 `Esc` 收起）及本机桌面程序快更全部通过；未连真实激光硬件实测（`unverified`）。
- **关联**：`frontend/src/components/{MaterialModal,ApprovalCenter,AiPreviewCenter,CloseConfirmModal,SettingsModal,Navbar,SideNav,AppDialog,SendOverlay}.jsx`、`frontend/src/components/{workbench/WorkbenchMaterialCard,material-lab/LabParamsPanel}.jsx`、`frontend/src/pages/{TwinPage,HomePage,DrawPage}.jsx`、`frontend/src/styles/15-addenda-2026-11.css`、`docs/developer/frontend-behavior.md`。

### 语音链路四处提速：工具不等开场白、“停”早复核、`tts start` 提前、命令词短回答少等（2026-11-24）

- **现象**：大模型先说开场白再调工具时工具要等开场白合成完才跑；思考中一声“停”要等 700 ms 确认窗口才送识别；每轮首段音频到了才发 `tts start`、再等 180 ms 才起播；开着命令词抗噪时“第一个”“再来一次”这类短回答要多等最多 500 ms。
- **根因与修复**：`Turn::generate_split` 让工具与开场白合成并行（门禁不变）；确认窗口里声音之后安静 250 ms 就提前复核“停”；新增 `TurnEvent::SpeechSoon`，第一句交给合成时先发 `tts start`，没出声时补发 `tts stop`；命令词退出类不再只凭“一 / 再”就等，最多等到尾音送完。**行为变更**：自动拾音下“思考中接着说”的窗口早结束一个首句合成时间。
- **验证**：`verify.mjs --finish` 通过：全工作区 clippy / Rust 测试（`moss-voice-server` 410 项，新增 `tools_start_without_waiting_for_the_preamble_to_be_synthesized`、`tts_start_goes_out_while_the_first_sentence_is_still_synthesizing`、`a_short_stop_is_rechecked_as_soon_as_it_is_followed_by_quiet`，命令词测试补“第一个”等 4 例；时间相关的用例连跑 3 遍均通过），快更本机桌面程序。收益为估算，未在真机上测首声时间。
- **关联**：`crates/moss-voice-server/src/conversation.rs`（`generate_split`、`PendingSpeech`、`SpeechSoon`、`command_hint_fits`、`COMMAND_HINT_IPC_MARGIN`）、`session/input.rs`（`track_quiet`、`STOP_RECHECK_QUIET`）、`session/turn.rs`（`tts_started_at`）、`session.rs`（`speaking_after_tts_start_at`）；内置语音技术书 n115。

### 命令词不再把“暂停”“别停”当急停，打断工具与说“停”不再丢失（2026-11-24）

- **现象**：开着命令词抗噪（新安装默认开）时，走边框说“暂停”或说“别停”会急停复位；工具跑到一半被打断后大模型不知道工具已执行，可能再执行一遍；自动拾音时工具执行中说“停”没反应；另有合成失败设备什么都收不到、`<think>` 推理内容被念出来、粗筛可能剪掉停止工具等问题。
- **根因与修复**：“暂停”移出停止类命令词，尾音里的停止词改由识别文字判断（否定说法不算），只有思考中新的一阵声音才直接急停；打断时按“调用 + 结果”补记已开跑的工具；工具执行中用能量检测 + 识别只听“停”；粗筛永远保留 `laser_safe_action_tool`；另修合成失败给文字、备用音色不缓存、去掉 `<think>`、命令词事件截断插话、出错半句进历史。**安全门禁变更**：停止快路径只认整句停止，见安全规范 §4.1。
- **验证**：`verify.mjs --finish` 通过：全工作区 clippy / Rust 测试（`moss-voice-server` 407 项，新增 `a_stop_word_in_the_tail_of_the_utterance_is_judged_by_the_recognized_text`（去掉修复时“别停”确实被急停）、`a_turn_interrupted_while_its_tool_runs_still_records_the_tool_call`、`saying_stop_while_a_tool_runs_stops_even_without_realtime_barge_in`、`think_blocks_in_the_reply_are_neither_spoken_nor_kept_in_history` 等 9 项）、前端检查与浏览器测试、快更本机桌面程序。未在真机与真实命令词模型上复测。
- **关联**：`crates/moss-voice-server/src/session/{listen,input,turn}.rs`（`still_speaking`、`watches_for_stop`、`interrupted_tool_messages`）、`conversation.rs`（`command_hint_fits`、`generate`、`cached_speech`、`INTERRUPTED_TOOL_RESULT`）、`jev.rs`（`NEVER_FILTERED_TOOLS`）、`moss-voice-providers/src/text.rs`（`ThinkFilter`）、`moss-voice-local-proto`（`COMMAND_KEYWORDS`）；内置语音技术书 n114。

### 生成预览、检查机器、拍照看图拿到调用就先应一声，提示词引导短开场白（2026-11-24）

- **现象**：让小智生成预览、检查机器或拍照看图时，大模型第一轮只给工具调用不说话，垫音要等工具跑满 1 秒才说，加上识别和大模型的时间会有 2～3 秒完全安静；大模型开头那句不带逗号时，首段提速用不上。
- **根因与修复**：新增 `slow_tool_filler_delay`：这三类一定要几秒的工具在 `TurnGate` 放行、工具开跑时就说垫音，查询类仍等 1 秒；垫音改成“好，我生成下预览。”这类口头应答，新增拍照的 `SLOW_LOOK_FILLER`。**提示词变更**：`IMMUTABLE_SAFETY_RULES` 引导开头那句在头几个字就用逗号断开。垫音门禁（R3、控制、混合调用不说）与闸门不变。
- **验证**：`verify.mjs --finish` 通过：全工作区 clippy / Rust 测试（`moss-voice-server` 391 项，新增 `known_slow_tools_answer_at_once_while_quick_queries_stay_quiet`：400 ms 的生成预览在工具跑完前就说垫音、400 ms 的查询不说），快更本机桌面程序。未用真实大模型与真机测量首声时间。
- **关联**：`crates/moss-voice-server/src/conversation.rs`（`slow_tool_filler_delay`、`slow_tool_filler`、`SLOW_LOOK_FILLER`、`immutable_safety_rules!`）、`tests/chat/shortcut.rs`（`known_slow_tools_answer_at_once_while_quick_queries_stay_quiet`）；内置语音技术书 n113。

### 过程记录补上判停复盘数据，并提供统计抢话与白等的脚本（2026-11-24）

- **现象**：要决定是否接中文文本判停模型或做自适应等待，得先知道现在的句尾规则有多少次抢话、多少次白等，但过程记录只写了“开口前等多久”，没写用户之后有没有接着说，无从统计。
- **根因与修复**：过程记录新增 `user_continued`（判停后用户又出声时的阶段、是否合并、句尾规则与最终等待、距放行与开播的时间、估算停顿，与 `hold_decision` 同一轮次）；新增 `scripts/voice-bench/endpoint-review.mjs` 按三档统计疑似抢话、接住的接着说、白等与停顿分布，样本少于 30 轮不下结论。只加记录，判停与回答行为不变。
- **验证**：`verify.mjs --finish` 通过：全工作区 clippy / Rust 测试（新增 `process_trace_records_user_continuing_after_an_endpoint_decision`，`step_texts_say_what_each_step_does` 增加两条）、脚本单测 75/75（新增 `endpoint-review.test.mjs` 4 项）、快更本机桌面程序；本机还没有真实过程记录，统计结论待实际使用后再跑，未在真机上验证。
- **关联**：`crates/moss-voice-server/src/session/input.rs`（`trace_user_continued`）、`session/turn.rs`（`endpoint_wait`、`released_at`）、`session/trace.rs`、`scripts/voice-bench/endpoint-review.mjs`；内置语音技术书 n112。

### 新安装默认选用整链实测最准的本地语音组合，本地模型页区分判停模型与叠加功能（2026-11-24）

- **现象**：新安装的本地模型全部关着，顶部推荐识别还是 SenseVoice，与整链实测“机器噪声下 Qwen3-ASR 错字少一半”的结论不一致；“按场景选一套”在模型没下载时也会显示“✓ 正在用”。
- **根因与修复**：新增 `BuiltinLocal::new_install` / `VoiceSettings::new_install`，只在设置文件不存在或没有 `voice` 分区时使用，`BuiltinLocal::default()` 不变，**老用户（含没写 `builtin_local` 的）与损坏的设置文件行为不变**；没下载前判停退回能量判停、识别与合成用云端，“全链路连贯试跑”也照此退回并写明“本地××还没下载”（原先直接报未下载失败，和实际对话不一致）。前端推荐识别改为 Qwen3-ASR，组合第一套改名“推荐（新安装默认）”，正在用但缺模型时列出缺什么并给“下载缺的模型”。另外“拾音判停”组区分“几选一”（判停模型）与“叠加，可同时开”（命令词抗噪、前端降噪、语义判停）并写明能和谁一起用；小智全量补字库不再在界面显示，也不再记“用户取消过就不自动下载”的标记，内置服务每次启动时没装就在后台自动补齐。
- **验证**：`verify.mjs --finish`：全工作区 clippy / Rust 测试（`moss-voice-host` 53 项，含 `new_install_picks_the_measured_combo_but_existing_users_keep_theirs`、`n111_full_font_pack_always_auto_loads_even_after_old_dismissal`、全链路试跑退回断言）、前端 lint / 单测 390/390（新增 `modelPairingGuide.test.mjs`）/ 构建、浏览器测试 17/18 通过，`twin-render.browser.mjs` 为改动前就偶发失败的用例、单独重跑 2 次均通过；快更本机桌面程序通过。未在真机与真实下载上验证。
- **关联**：`crates/moss-voice/src/settings.rs`（`new_install`、`load`、`parse_voice_section_checked`）、`frontend/src/voiceState/modelSources.js`（`RECOMMENDED_LOCAL_MODELS`、`MODEL_COMBOS`、`comboMissingModels`）、`frontend/src/components/voice-builtin/{ModelPairingGuide,LocalModelsGroup,BuiltinScreenPreview}.jsx`、`crates/moss-voice-host/src/builtin/{wakeword,models}.rs`（`ensure_full_font_ready_with_base`）、`crates/moss-voice-host/src/builtin/chain_test.rs`、`frontend/tests/modelPairingGuide.test.mjs`；内置语音技术书 n111。

### 优化数字孪生、创作画板确认框及全站提示文案（2026-11-23）

- **现象**：① 数字孪生 3D 视口把滚轮缩放挂在 React 合成 `onWheel`（passive 监听），缩放时会连带滚动整页；历史任务与实时下发状态直接裸打英文（`preview_ready`、`SENDING`），底部推演栏重复展示循环与时长，右侧卡片堆满 `?` / `Bf` / `RX` / `Pn` / `$CD` 黑话并写死翼宿 V1.0 硬件规格；② 创作画板「确认开始雕刻」弹窗缺少 `Esc` / 遮罩关闭与 Portal 顶层挂载；③ 任务进度浮层、材料实验室、配对弹窗、顶栏提示、底部控制台与语音中心扩展卡仍残留 `job_id` / `workflow_id` / `calibration_id` / `Token` / `HashFS` / `moss-host` / `ONLINE` / 顶栏本地路径等研发词与重复长段。
- **根因与修复**：① `TwinStage3D` 改用原生 `addEventListener("wheel", ..., { passive: false })` 并在拖拽与缩放时清掉视角过渡目标，去掉写死机型标识；`TwinPage` 复用 `normalizeWorkflowStatus` 做中文状态映射，删除重复的 `#twin-loop-status-pill` 与冗余时长，卡片 2/3 去黑话并移除固定翼宿硬件格；② `SendConfirmDialog` 改为 `createPortal(..., document.body)` 挂载并补齐 `Escape`（经 `isTopmostModalBackdrop` 判顶层、忙态锁定）与防拖拽误触遮罩关闭，`07-pages.css` 同步按钮样式并去掉 `HashFS`；③ 清理 `JobProgressDock`、`MaterialLabPage`（路径移入悬停提示）、`AuthModal`、`Navbar`、`ConsoleDrawer`、`VoiceAgentCard`、`VoiceMcpCard` 里的内部字段名与冗长说明。不改任何接口契约、安全门禁与创作画板后端 100 mm 默认幅面护栏。
- **验证**：前端 ESLint、单测 389/389、生产构建、`check-crate-layers`、`check-repo-hygiene`、浏览器测试 18/18（`twin-render.browser.mjs` 新增滚轮缩放不滚页断言，`usability-layout.browser.mjs` 新增确认框 Portal 与 `Esc` 关闭断言）、回环 E2E 41/41 及 `scripts/quick-update-desktop.sh` 快更全部通过；未连真实硬件实测。
- **关联**：`frontend/src/components/TwinStage3D.jsx`、`frontend/src/pages/{TwinPage,MaterialLabPage}.jsx`、`frontend/src/components/draw/SendConfirmDialog.jsx`、`frontend/src/components/{JobProgressDock,AuthModal,Navbar,ConsoleDrawer,VoiceAgentCard,VoiceMcpCard}.jsx`、`frontend/src/styles/07-pages.css`、`frontend/tests/{twin-render,usability-layout}.browser.mjs`。

### 修复连接弹窗改完即关丢输入的同步竞态，并精简语音中心其余卡片文案（2026-11-23）

- **现象**：① 在统一连接弹窗填完设备 IP 后紧接着按 `Esc` 关闭，AI 预览窗偶发仍显示未配置（`ai-preview.browser.mjs` L218 复现）；② 语音中心“当前语音链路”脚注、云端 Jev、视觉理解、声纹、已连接设备、家居备份与 USB 配网仍残留长段重复说明及过时 / 底层词（“协议 6”“`aec` 字段”“DPAPI / SQLite/WAL/SHM”“NVS 分区”）。
- **根因与修复**：① `useDeviceConnection` 原先在异步 `useEffect` 里落盘并广播 `moss:device-connection`，弹窗在 `Esc` 同步卸载时可能跳过副作用导致丢改动；改为在各字段 setter 内同步更新 `persistedRef`、写入存储并广播事件，挂载时仍不回写旧值；② 同步精简 `BuiltinModelGroups`、`BuiltinJevGroup`（去掉不可改的固定地址行）、`VoiceVisionCard`（本地视觉不提示云端费用）、`VoiceSpeakerCard`（去掉过时的“协议 6”）、`BuiltinDevicesPanel`、`VoiceHomeCard` 与 `XiaozhiUsbProvisionCard` 文案。不改任何设置键、安全门禁与接口契约。
- **验证**：前端 ESLint、单测 389/389、生产构建、`check-crate-layers`、`check-repo-hygiene`、浏览器测试 18/18（`ai-preview.browser.mjs` 首次即过，`twin-render` 复跑通过）、回环 E2E 41/41（含 `[UI-07]` 统一连接跨页同步）及 `scripts/quick-update-desktop.sh` 快更全部通过；未连真实硬件实测。
- **关联**：`frontend/src/useDeviceConnection.js`、`frontend/src/components/{VoiceVisionCard,VoiceSpeakerCard,VoiceHomeCard}.jsx`、`frontend/src/components/voice-builtin/{BuiltinModelGroups,BuiltinJevGroup,BuiltinDevicesPanel,XiaozhiUsbProvisionCard}.jsx`。

### 精简本地模型页与偏好设置的文案说明，修正与推荐结论冲突的识别模型描述（2026-11-23）

- **现象**：“本地模型（断网可用）”的模型名称与说明里重复写体积、堆砌开源协议与量化/引擎术语、硬件加速和默认关闭来回说三四遍，且 `Qwen3-ASR` / `FireRedASR2-CTC` 仍写“高于 SenseVoice”与顶部推荐矛盾；偏好设置里“完整过程记录”正文和确认弹窗大段重复、“顶栏状态胶囊”与复选框互相复述、端口与退出含“上位机 / 同源通信 / 杀死主进程”黑话，且关闭提示在 macOS 仍写“右上角”。
- **根因与修复**：① 精简 `moss-voice::local_models::CATALOG` 全部 27 条模型的 `label` / `note`（去掉与大小列重复的体积和无关协议/量化词，保留 `ZipVoice` 仅限非商用与 `TEN-VAD` 竞品限制，按实测重述 `Qwen3-ASR` / `FireRedASR2-CTC`），收短 `LocalModelsGroup`、`ColabBridgeSection` 的重复提示与胶囊、将存放目录移至底部下载地址下，并精简 `LocalTtsVoicesModal` 音色列表（与语言包相同的试听语不再逐行重复，全部缓存后隐藏逐行“本机已缓存”标签）；② 精简 `ProcessTraceSection`（明细只留开启确认框）、`SettingsModal`（端口改大白话、关闭说明适配 macOS/Win、去顶栏重复段、Jev 补“模型：”标签、保留底部安全底线）与 `PhoneLinkSection`。不改任何设置键、确认弹窗内容与后端契约。
- **验证**：`rustfmt`（仅 `local_models.rs`）、`cargo clippy/test -p moss-voice`（99 通过）、前端 ESLint、单测 388/388、生产构建、`check-crate-layers`、`check-repo-hygiene` 及 `scripts/quick-update-desktop.sh` 快更全部通过；浏览器测试 `voice-center` / `app-update` / `usability-layout` / `shell-chrome` 等 17/18 通过（`ai-preview.browser.mjs` L218 在本次未改动的文件上出现既有同步竞态）。未连真实设备实测。
- **关联**：`crates/moss-voice/src/local_models.rs`、`frontend/src/components/voice-builtin/{LocalModelsGroup,ColabBridgeSection}.jsx`、`frontend/src/components/{LocalTtsVoicesModal,SettingsModal,ProcessTraceSection,PhoneLinkSection}.jsx`、`frontend/tests/voice-center.browser.mjs`。

### 本地模型页的“Colab T4 专属联动”改为默认收起的折叠栏（2026-11-23）

- **现象**：“本地模型（断网可用）”里“Colab T4 专属联动”默认整块展开，未开启时仍占约半屏高度，压低下方本地模型包的可见区域。
- **根因与修复**：`ColabBridgeSection`（`#voice-local-colab-bridge`）改为 `<details class="voice-fold">` 折叠栏，标题栏保留状态胶囊 `#voice-colab-state-pill` 与未保存标记，导出笔记本按钮移至展开区顶部右侧；新增 `colabBridgeDefaultOpen`，未开启时默认收起，开启、接管中、正在拉起 / 运行或有拉起报错时自动展开（手动点开 / 收起后以用户为准）；`VoiceLayout::focusVoiceGroup` 补齐自动展开目标 `<details>`，从视觉理解或顶部链路栏点“去联动开关 / 去配置”可直达展开态。只改展示，配置、开关默认值与后端契约不变。
- **验证**：`voiceState.test.mjs` 补 `colabBridgeDefaultOpen` 各状态断言，`voice-center.browser.mjs` 补默认收起、手动展开 / 收起及接管中点顶部“去配置”自动展开；通过 `check-crate-layers`、`check-repo-hygiene`、脚本单测 71/71、前端 ESLint、单测 388/388、生产构建、浏览器测试 18/18 及 `scripts/quick-update-desktop.sh` 快更（未改 Rust 代码且为避开并行会话的未提交 `.rs` 文件，未跑 `verify.mjs --finish` 的整仓 `rustfmt`/Rust/E2E）；未连真实 Colab T4 实测。
- **关联**：`frontend/src/voiceState/localModels.js`（`colabBridgeDefaultOpen`）、`frontend/src/components/voice-builtin/ColabBridgeSection.jsx`、`frontend/src/components/VoiceLayout.jsx`、`frontend/src/styles/15-addenda-2026-11.css`、`frontend/tests/{voiceState.test.mjs,voice-center.browser.mjs}`、`docs/developer/frontend-behavior.md`。

### 完整过程记录支持选择保存位置、恢复默认与打开文件夹（2026-11-23）

- **现象**：过程记录只能写到运行目录，录音与照片占用较多空间时用户无法改存其他磁盘，也不能直接从设置打开记录文件夹。
- **根因与修复**：桌面主窗口新增三个原生命令并同步全平台 Tauri 权限清单，选择后使用带归属标记的 `moss-process-trace` 子目录（兼容 `.DS_Store`、剥离 Windows `\\?\` 前缀，并在元数据访问前拦截 UNC 共享路径、软链祖先与 `..` 段以防 SMB 阻塞），`process_trace_dir` 仅原生入口保存、HTTP 拒绝路径写入。新记录即时切换，接口起止、对话轮次（`ActiveTurn.trace` 固定句柄）、视觉看图与后台模型调用的附件和时间线均留在同一目录；旧历史不迁移且不再自动清理。清空、到期清理与占用统计只处理 MOSS 记录，目录失联暂停、接回实时恢复且不静默回退。默认关闭、默认路径、30 天保留期及移动端行为不变。
- **验证**：覆盖 `laser-runtime` 自定义目录单测（防嵌套、非空未标记同名目录拒绝、UNC 与 `..` 入口拒绝、跨目录切换附件一致性与旧目录免清理、无关内容与软链保护且清空归零、拔盘暂停与接回恢复）、`laser-api` HTTP 拒绝路径写入、`moss-voice-server` 轮次中途换目录归档单测及 `app-update.browser.mjs` 交互用例；通过 `node scripts/verify.mjs --finish` 全工作区检查、浏览器测试 18/18、回环 E2E 41/41 并快更本机 `/Applications/MOSS.app`；真实外接盘插拔与 Windows 原生选择框未实测（`unverified`）。
- **关联**：`crates/laser-runtime/src/process_trace.rs` 与目录模块、`crates/laser-api/src/{process_trace.rs,routes/system.rs}`、`crates/moss-voice-server/src/{session/*,conversation.rs,state.rs,vision.rs}`、`apps/desktop/src-tauri/{src/process_trace.rs,src/export.rs,gen/schemas/*}`、`frontend/src/components/ProcessTraceSection.jsx`；融合技术书 r62。

### 完整过程记录补齐全流程、按真实先后、不限硬盘大小，脱敏只换凭据本身（2026-11-23）

- **现象**：完整过程记录未覆盖本地模型阶段（降噪、判停方式与起止、命令词改判、语义判停概率、中间识别与忽略原因），大模型回复在抢跑等待期被冻结或随打断丢失、激光接口仅在结束后补记；另有 2 GB / 7 天上限、512 条队列满即丢、4000 字截断，以及过度脱敏与凭据转义漏网。
- **根因与修复**：写入核心改为无界队列、取时刻与入队同锁、只按日期保留 30 天，附件失败或内存待写超 1 GiB 写明原因；**辅助程序协议 7→8（`Opened{denoise}`），须与桌面程序同步升级**；对话任务经 `TurnTrace` 在发生当刻直写 `asr` / `llm_request` / `llm_round` / `tool_call` / `tts_*`（不被 `turn.hold` 冻结或打断丢弃），`VadWorker` 首帧前 `reset` 不重复开流双记降噪，按键说话不虚报 `vad_engine`、非抢跑记明跳过语义判停原因，补齐 `SpeechCppWorker` 与音色试听缓存进程的 `helper_*` 起止记录并修正回声模式 `service_start` 挂载状态；激光拆 `api_begin` / `api_call`（急停先执行后补记、画板自动同步只记摘要、大报文单次解析脱敏另存）；脱敏修正 `Authorization: Bearer` 漏遮、转义引号、`wss://` 全查询串与 `sk-` 快速路径。
- **验证**：覆盖写入核心（无界队列、内存高水位、附件失败）、激光（急停先执行后补记、大报文单次脱敏另存、`Bearer` / 转义引号 / `wss://` 脱敏样例）、语音（首轮单次降噪、抢跑 `hold` 实时落盘与打断保留、中途开启记录补写判停、语义判停跳过原因、辅助程序启停）与协议 8 往返单测；通过 `node scripts/verify.mjs --finish` 全工作区检查、浏览器测试 18/18、回环 E2E 41/41 并快更本机桌面程序（含协议 8 辅助程序与 `moss-host`）；未连接真实小智硬件实测（`unverified`）。
- **关联**：`crates/laser-runtime/src/process_trace.rs`、`crates/laser-api/src/{process_trace.rs,routes.rs,send.rs,feedback.rs}`、`crates/moss-voice-server/src/{session/*,conversation.rs,audio/*}`、`crates/moss-voice-providers/src/{local.rs,speech_cpp.rs}`、`crates/moss-voice-local-proto`、`apps/moss-voice-local/src/main.rs`、`crates/moss-voice-host/src/builtin/{conversation,lifecycle,tts_preview}.rs`、`frontend/src/{processTrace.js,components/ProcessTraceSection.jsx}`；内置语音技术书 n110、融合技术书 r61。

### 本地模型页把判停相关四张表并成一组，实测无提升的模型默认收起（2026-11-23）

- **现象**：“本地模型（断网可用）”里拾音判停、前端降噪、命令词抗噪、语义判停各占一张“模型包”表，共 7 个模型，其中多数经整链实测没有提升却和常用模型并列，页面冗长；降噪有两个模型但没有选用入口，DPDFNet 下载后用不上。
- **根因与修复**：新增 `localModelLayout`：命令词抗噪作为“判停增强”并入“拾音判停”组，老版 Silero、TEN-VAD、两个前端降噪与语义判停收进组内默认收起的“更多判停模型与实验功能”，开着或正在用、正在下载时自动展开；顶部判停下拉选了收起区未下载的模型时经 `revealLocalMore` 先展开再定位；降噪表新增“选用”。只改展示，模型清单、设置默认值与契约不变。
- **验证**：`voiceState.test.mjs` 补版面拆分与自动展开条件断言，`voice-center.browser.mjs` 补并组、收起 / 展开与降噪“选用”；收尾级 `node scripts/verify.mjs --finish` 通过（前端单测 388 项、浏览器测试 18/18），已快更本机桌面程序；未连真实设备实测，模型推理行为未改动。
- **关联**：`frontend/src/voiceState/localModels.js`（`localModelLayout`、`isFoldedLocalModel`）、`frontend/src/components/voice-builtin/{LocalModelsGroup.jsx,BuiltinModelGroups.jsx,shared.js}`、`frontend/src/styles/09-voice-center.css`、`frontend/tests/{voiceState.test.mjs,voice-center.browser.mjs}`、内置语音技术书 n109。

### 唤醒词改显示文字时拼音自动跟着更新（2026-11-23）

- **现象**：设备唤醒词卡里改了“显示文字”后，拼音框不变，需要用户自己逐个音节手打，容易漏改或拼错。
- **根因与修复**：新增按需加载的 `wakePinyin.js`（`pinyin-pro` 词组词库判断多音字，MOSS 读 mo si、数字按中文读、ü 写 v），`applyWakeText` 在拼音为空或仍是旧文字自动结果时跟随更新，手动改过的不覆盖，并提供“按文字重新生成”按钮；保存契约与后端音节校验不变，新增前端依赖 `pinyin-pro` 3.29.5（MIT，单独分包）。
- **验证**：`voiceState.test.mjs` 新增转拼音（多音字、MOSS、ü、数字）与跟随 / 不覆盖两项，`voice-center.browser.mjs` 补自动跟随、手动不覆盖、按文字重新生成；收尾级 `node scripts/verify.mjs --finish` 通过（前端单测 388 项、浏览器测试 18/18、生产构建拼音词库单独分包约 304 KB），已快更本机桌面程序；未在真机上实测新唤醒词识别效果。
- **关联**：`frontend/src/{wakePinyin.js,voiceState/devices.js,components/VoiceWakeWordCard.jsx}`、`frontend/src/styles/13-voice-devices.css`、`frontend/tests/{voiceState.test.mjs,voice-center.browser.mjs}`、内置语音技术书 n108。

### 工作台新增“现场移头与对刀原点”折叠面板，数字孪生遥测卡补齐连接摘要（2026-11-23）

- **现象**：机器上电锁轴后无法手推微调对刀，而界面原先只在已生成预览后的「两点对位」里藏有点动，常规摆料缺少独立的移头与设原点入口；数字孪生「控制器实时遥测」卡不显示当前目标 IP/串口，也无修改连接入口。
- **根因与修复**：① 数字孪生遥测卡顶部接入 `connectionSummaryView` 连接摘要与「修改连接」按钮，唤起统一的 `DeviceConnectionModal`；② 工作台加工监视舱顶部新增默认收起的 `WorkbenchHeadJogPanel`（折叠状态记入 `moss_ui_fold_wb-head-jog`），提供 `1 / 5 / 10 mm` 单步 `X±/Y±` 点动、「设为工作原点」与「解锁 `$X`」（无连续点动、无出光），完全复用 `jogRequest`、`estopTargetArgs`、`showConfirm` 与后端 `/api/safe-action` 确认及严格模式 R3 审批门禁；未连接、未握手探测、加工中或在途执行时全部按钮禁用并说明原因，同步锁拦截快速连点，展开不挤出 `1280×760` 首屏操作与急停。后端接口不变，详见融合技术书 r60。
- **验证**：`registrationState.test.mjs` 与 `uiPolishB.test.mjs` 新增禁用条件、10mm 步长上限、二次确认、在途防连点、严格模式 202 审批及组件 SSR 断言，同步补写 `{usability-layout,twin-estop}.browser.mjs` 与 `e2e/workbench.ui.test.mjs` `[UI-08]`；收尾级 `node scripts/verify.mjs --finish` 通过（前端单测 386 项、浏览器测试 18/18、release `moss-host` 回环 E2E 41/41），已快更本机桌面程序；`[UI-08]` 弹窗“取消”选择器与关闭钮同名，已改为点底部取消按钮。未连真实硬件（`unverified`）。
- **关联**：`frontend/src/{registrationState.js,main.jsx}`、`frontend/src/components/workbench/{WorkbenchHeadJogPanel,WorkbenchMonitorCard}.jsx`、`frontend/src/pages/{HomePage,TwinPage}.jsx`、`frontend/src/styles/15-addenda-2026-11.css`、`docs/developer/frontend-behavior.md`、`docs/user/quick-start.md`、融合技术书 r60。

### 界面继续整理：实验室功率统一为百分比、去掉工程术语、语音中心首屏压缩与配通清单、设置摘要、反馈快捷症状与对刀起点图示（2026-11-23）

- **现象**：① 材料实验室「校准矩阵」的测试类型与列/行参数下拉嵌在带省略号的 `<label>` 内被截断，「材料别名」只占半列，且激光功率写内部标尺 `0–1000` 与工作台/画板的 `0–100%` 割裂；② 侧栏导航与卡片头重复堆叠英文代码徽标，画板工具栏与侧栏重复放置「工作台」按钮，终端空态、卡内急停与语音状态卡夹杂工程术语；③ 语音中心非对话时全息状态卡占高过大，内置链路新装机缺少一眼可见的配通步骤；④ 偏好设置折叠行需逐个点开才能看到当前配置，Jev 填新密钥点测试会被要求先手动点保存；⑤ 工作台无刀路时「加工反馈」展开占高且需手打常见缺陷词；⑥「木牌中心」与「左下角」对刀起点仅靠文字区分，开刻前易看漏。
- **根因与修复**：① 校准矩阵标签与下拉改为兄弟节点，「材料别名」跨两列；新增 `PowerPercentInput` 与 `powerScaleToPercent` / `percentToPowerScale`，实验室功率前端统一为 `0–100%`（一位小数往返无漂移，旁注 S 值），后端存储与 API 契约（`0–1000`）不变；② 去掉侧栏英文徽标、卡片头英文小标与画板重复的「工作台」按钮，卡内急停改为「紧急停止（软件复位）」（id 与行为不变）；③ `MossVoiceTwin` 非对话时压成单行，`VoicePage` 内置链路未就绪时显示只读三步配通清单；④ `FoldHeader` 右侧显示当前值摘要，Jev「保存并测试」先保存再测、失败即止；⑤ 加工反馈无刀路时收起，新增 5 个症状快捷胶囊；⑥ 新增 `OriginBadge` 对刀起点示意图，用于工作台、画板与两处发送确认框。后端接口与发送确认门禁不变，细节见 `frontend-behavior.md` 与融合技术书 r59。
- **验证**：新增 `frontend/tests/uiPolishB.test.mjs` 并在 `frontend/tests/{materialLabState,voiceState,targetedFixes}.test.mjs`、`frontend/tests/{usability-layout,voice-center}.browser.mjs`、`frontend/tests/e2e/workbench.ui.test.mjs` 补充与同步断言（含 `0–1000` 全整数往返零漂移、三步清单状态流转、Jev 先存后测拦截、症状胶囊去重与五处起点图示）；收尾级 `node scripts/verify.mjs --finish` 通过（前端单测 386 项、浏览器测试 18/18、release `moss-host` 回环 E2E 41/41），已快更本机桌面程序；收尾时修正 3 个浏览器测试的假数据与时序（期望不放宽）。未连接真实硬件（`unverified`）。
- **关联**：`frontend/src/materialLabState.js`、`frontend/src/components/{material-lab/PowerPercentInput,material-lab/LabMatrixSettings,SideNav,FoldHeader,MossVoiceTwin,SettingsModal,FeedbackPanel,OriginBadge}.jsx`、`frontend/src/components/{workbench,draw}/*`、`frontend/src/pages/{HomePage,DrawPage,MaterialLabPage,VoicePage}.jsx`、`frontend/src/styles/`、`docs/developer/frontend-behavior.md`、`docs/user/quick-start.md`、融合技术书 r59。

### 验证改为迭代 / 收尾 / 交付三级并交给脚本执行，CHANGELOG 与技术书修订记录归档瘦身（2026-11-23）

- **现象**：规则要求每次代码改动都跑全套（前端构建、浏览器测试、release `moss-host` + E2E、快更），多轮任务会重复好几遍；跑哪些检查、带哪些快更参数要模型逐条判断；`CHANGELOG.md` 达 980 KB、两本技术书修订记录共约 25 万字，每次改动都要读写这些大文件。
- **根因与修复**：`verify.mjs` 新增 `--affected`（只对改动 crate 跑 clippy / test，前端不构建）与 `--finish`（`planFinish` 按改动路径自动追加浏览器测试 / E2E，通过后按平台快更并自动带 `--with-host` / `--with-voice-helper`，`--no-update` 可跳过），结尾打印通过摘要；新增 `archive-changelog.mjs`，`release.yml` 发布说明在根文件找不到版本段时读 `docs/changelog/`；`check-repo-hygiene.mjs` 检查新条目四项格式、每项不超过 600 字符、根文件已发布版本段不超过 1 个。`AGENTS.md` 按官方建议重写：去掉强调语气、写明原因、删去脚本已保证的规则，并授权收尾验证后本地提交。
- **验证**：`scripts/tests/{verify,repo-hygiene,archive-changelog}.test.mjs` 新增 13 项、`release-gates.test.mjs` 补归档回退断言，脚本单测 71 项与仓库规范检查通过，`--affected` / `--finish` 的 `--dry-run` 计划核对无误。未改运行时代码，不需要快更；`release.yml` 的归档回退只在本地模拟验证。
- **关联**：`scripts/{verify,archive-changelog,check-repo-hygiene}.mjs`、`.github/workflows/release.yml`、`AGENTS.md`、`docs/changelog/`、`docs/integration/revisions/`、`docs/developer/{standards,packaging-install}.md`、`docs/README.md`。

### 前端易用性整理：主操作首屏可见、全站统一设备连接面板、标定向导可收起与界面细节收敛（2026-11-23）

- **现象**：① `1280×760` 与 `1440×900` 视口下，雕刻工作台未生成刀路时加工监视舱被大块空黑框撑高，「计算加工轨迹 / 开始雕刻 / 0x18 急停条」掉出首屏需下滚，创作画板画完也需滚过整屏才能点「生成预览」与「确认发送」；② 工作台、画板与材料实验室各自维护独立连接表单、串口枚举与波特率列表，顶栏「硬件」胶囊点击只跳回工作台且未探测时误显红色故障灯，AI 预览窗未配设备时无法就地打开连接配置；③ 材料实验室首次标定向导常驻占约半屏把材料库挤出首屏，步骤说明重复且「跳过这一步」单占一行；④ 自由画布布局提示与锁定/还原按钮每页占满整行，`1280px` 宽下控制器 IP 占位符与对刀起点下拉文字截断，语音中心把设备说明与严格模式警告塞进同一条提示无法分别关闭且内置链路漏显严格模式警告。
- **根因与修复**：① 工作台监视舱空态改为可压缩高度，`.job-dispatch-card` 吸底常驻（展开两点对位时暂停）；画板未手动拉高时 `board` 卡限制在首屏内，新增吸底操作条 `#draw-board-action-bar`，沿用原确认与刀路 SHA-256 门禁；② 新增全局 `DeviceConnectionModal`，顶栏硬件胶囊、各页「修改连接」与 AI 预览窗共用，连接配置收口到单一 `laser-forge-*` 存储源，打开和切换时不自动探测（启动且已有保存目标时只做一次 600ms 静默握手），顶栏未探测态改为中性灰；③ `CalibrationWizard` 可整体收成单行摘要；④ `WorkbenchCanvas` 布局控制收进顶栏浮层，任何宽度下 `#nav-estop` 都可见可点；语音中心设备提示与严格模式警告拆成两条可分别关闭的提示。后端接口与安全门禁不变，细节见 `frontend-behavior.md` 与融合技术书 r58。
- **验证**：新增 `tests/usability-layout.browser.mjs`（含窄屏急停 `elementFromPoint` 命中、画板首屏不遮挡、启动单次静默握手），并在 7 个单测文件、4 个浏览器测试与 `tests/e2e/workbench.ui.test.mjs` 补充断言；前端 lint / test / build / test:browser、`verify.mjs --fmt-write` 与 release `moss-host` + `test:e2e` 全部通过。未连接真实激光硬件（`unverified`）。
- **关联**：`frontend/src/{main.jsx,deviceConnection.js,useDeviceConnection.js,workbenchLayout.js,state/hardwareStatus.js}`、`frontend/src/components/{DeviceConnectionModal,Navbar,WorkbenchCanvas,CalibrationWizard,AiPreviewCenter}.jsx`、`frontend/src/components/{workbench,draw,material-lab}/*`、`frontend/src/pages/{HomePage,DrawPage,MaterialLabPage,TwinPage,VoicePage}.jsx`、`frontend/src/styles/{06-shell,15-addenda-2026-11}.css`、`docs/developer/frontend-behavior.md`、`docs/user/quick-start.md`、融合技术书 r58。

### 修复审查发现的 VPN 地址抢先、配网串口列表撞锁、过程记录时间戳与丢记录、设备工具超时误报与模型删除越界等问题（2026-11-22）

- **现象**：① 全隧道 VPN 网卡名未命中虚拟关键字（如 `NordLynx`、`OpenVPN TAP-Windows6`、`tun0`、`ppp0`、`ipsec0`）并接管默认路由时，`sort_lan_candidates` 将其排在真实有线/无线网卡之前写进板子 OTA 地址导致连不上；② USB 配网切回窗口静默刷新串口列表（`ports`）也争抢 `usb_provision_lock`，与“读取板子配置”互撞误报 409 `usb_provision_busy`；③ 完整过程记录在后台写线程落盘时才取本地时间，慢盘或清理阻塞期间事件时间集体滞后且 23:59:59 的事件会落入次日目录，队列满静默丢记录无痕迹，且附件队列只限条数不限在途字节，`api_call` 在请求结束时写入排在其触发的 `generate` / `job_start` 之后缺少开始时刻；④ 设备端 MCP 初次握手或拉取工具列表超时后把 `device_tools_ready` 置为 `true`，后续轮询不再重试，唤醒词与摄像头页持续误报“固件不支持”直至设备重连，且原有 `SyncTools` 去重测试未真正卡住去重分支；⑤ 本地模型 `delete()` 会把所有只读回退目录里的同名模型一并删掉，且回退候选复用了排除 `\bin` 的新下载过滤导致旧版 `...\bin\models` 未被识别；⑥ 小智控制台提示词压缩后漏掉“出光/改GRBL参数”执行前确认项。
- **根因与修复**：① `ota::sort_lan_candidates` 改为仅对可识别物理网卡（`name_rank` 0–2）给予默认出网口加分，未知网卡名的默认路由不再压过有线/无线；② `BuiltinVoice::usb_provision` 对只枚举不打开串口的 `UsbProvisionRequest::Ports {}` 跳过 `usb_provision_lock`，读写板子仍独占该锁；③ `ProcessTrace::record` 改为在入队时采样本地时间决定 `at` / `at_ms` 与日期目录（`seq` 仍由写线程按落盘顺序分配），新增 256 MiB 在途附件字节预算（`MAX_INFLIGHT_FILE_BYTES`，RAII 守卫释放）与原子丢弃计数，队列满或超在途字节预算丢弃后下一条成功落盘记录附带 `dropped_before: N` 并在 `timeline.txt` 行末追加中文提示；`laser-api` 的 `api_call` 新增请求开始时刻 `started_at`（本地时间，格式同 `at`）；④ `Session::prune_stalled_device_mcp` 在 MCP 握手或 `tools/list` 超时时仅重置在途标志并释放滞留抓拍票据，保持 `device_tools_ready = false` 供后续 `SyncTools` 重试且同一时刻最多一轮握手在途；另补说明：`Session::end_conversation` 中 `finish_timing("disconnected")` 现于清空 `self.turn` 前执行，即使完整过程记录关闭，常连结束时未收尾的轮次也会产生 `disconnected` 耗时记录（与 WebSocket 断开路径一致）；⑤ `ModelManager::delete` 改为按候选顺序只删除当前正在使用的第一个已安装副本，保留其余回退目录副本；`paths::resolve_model_roots_from` 的回退候选改用 `fallback_exe_dir_from`（仅排除临时目录、`target\` 与 `.app\Contents\MacOS`），保留已存在的 `...\bin\models` 作只读回退，新下载目录过滤不变；⑥ `xiaozhi-prompts/latest.md` 补回“出光/改GRBL参数”确认要求（1969/2000 字）并新增历史快照。安全门禁与默认开关不变。
- **验证**：新增或收紧 `ota::tests::lan_ip_sorting_prefers_default_route_then_wired_over_wifi`、`builtin::usb_provision::tests::usb_provision_ports_does_not_contend_with_flash_lock_while_inspect_returns_busy`、`process_trace::tests::{record_uses_enqueue_timestamp_instead_of_delayed_flush_time,queue_overflow_reports_dropped_before_in_next_record_and_readable_timeline,inflight_file_byte_budget_rejects_excess_attachments_and_counts_drops}`、`laser_api::process_trace::tests::api_call_records_started_at_timestamp_when_request_begins`、`tests::chat::mqtt::{device_mcp_timeout_keeps_tools_unready_and_retries_on_sync_tools,sync_tools_deduplicates_before_init_and_hello_without_mcp_clears_loading_state}`、`local_models::tests::delete_removes_only_active_copy_when_both_primary_and_fallback_have_model`、`builtin::paths::tests::nsis_install_uses_runtime_models_and_falls_back_to_legacy_install_and_localappdata`；`node scripts/verify.mjs --fmt-write`、`cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo test --workspace` 全部通过。未连接真实小智硬件、VPN 隧道环境与激光机实测（`unverified`）。
- **关联**：`crates/moss-voice-server/src/{lib,ota,session,session/connect,session/device,session/lifecycle,tests/chat/mqtt}.rs`、`crates/moss-voice-host/src/builtin/{usb_provision,paths}.rs`、`crates/laser-runtime/src/process_trace.rs`、`crates/laser-api/src/{routes,process_trace}.rs`、`crates/moss-voice/src/local_models.rs`、`docs/integration/xiaozhi-prompts/{latest.md,history/2026-11-22-confirm-laser-grbl.md}`、`docs/developer/{api-contracts,packaging-install}.md`、技术书 n107、融合技术书 r57。

### 收口本地语音模型存放路径至运行数据目录，支持历史安装目录双向回退与 `Windows.old` 重装提醒（2026-11-22）

- **现象**：Windows 普通安装版（如 `%LOCALAPPDATA%\MOSS` 或自定义可写目录）此前将 `settings.json` 存放在 `%APPDATA%\moss`，却把数 GB 本地模型下载进程序目录 `<安装目录>\models`；用户卸载保留数据后换目录重装、切换安装版与便携版/`start.bat` 时，已保存的本地模型全部显示“未下载”并导致内置语音自启失败，勾选“删除应用程序数据”卸载时又把模型残留在旧程序目录；此外 Windows 系统重置把旧 `%APPDATA%\moss` 移入 `C:\Windows.old` 后无任何提示，满 10 天会被系统自动清空。
- **根因与修复**：`paths::resolve_model_roots` 识别含 `uninstall.exe` 的 NSIS 安装目录（以及 Unix `bin` 与系统临时目录），统一将新下载模型与新建 Laya 环境收口到运行数据目录 `%APPDATA%\moss\models`（仅无 `uninstall.exe` 的独立免安装便携版默认写程序同目录 `models/`，且可写探测采用带进程号序号的独立探针并缓存，不提前建空 `models/`）；`ModelManager` 新增 `with_fallback_roots` / `write_root_for`、`VoiceSettings::builtin_{section,chat}_ready_in` 与 `HostHooks::builtin_chat_ready`，自动对已存在的 `<运行目录>/models`、`<程序目录>/models` 及 `%LOCALAPPDATA%\MOSS\models` 做双向只读回退识别、链路切换就绪判定、按实际目录定位推理运行时与试听缓存，并支持全量补字库跨目录重下后热切换（`GlyphFontService::set_full_dir`），存量模型零迁移直接可用；新增 `detect_windows_old_moss`，分别识别同名用户 `Windows.old` 下 `Roaming\moss` 与 `Local\MOSS\models` 中的旧配置与旧模型，缺失期间写入诊断警告并在全部迁回后自动静默。默认值、接口契约与安全门禁不变。
- **验证**：新增 `moss_voice::local_models::tests::fallback_roots_recognize_legacy_installs_while_new_downloads_use_primary_root`、`moss_voice::center::tests::mode_changes_uses_host_hook_for_fallback_or_colab_readiness`、`moss_xiaozhi_display::glyph_font::tests::undisplayable_chars_lists_only_what_neither_side_can_draw`（热切换）、`moss_voice_host::builtin::paths::tests::{nsis_install_uses_runtime_models_and_falls_back_to_legacy_install_and_localappdata,windows_old_detection_warns_only_while_current_models_or_config_missing}`；`node scripts/verify.mjs`、全工作区 `cargo clippy` 与 `cargo test --workspace` 通过，已同步本机 `/Applications/MOSS.app`。未在真实 Windows 安装器卸载换盘流程下实机手测（`unverified`）。
- **关联**：`crates/moss-voice/src/{local_models,settings,center}.rs`、`crates/moss-voice-host/src/{builtin.rs,builtin/{paths,lifecycle,laya_jev,status,speaker,wakeword,models}.rs}`、`crates/moss-xiaozhi-display/src/glyph_font.rs`、`crates/moss-voice-server/src/lib.rs`、`apps/desktop/src-tauri/src/voice.rs`、`docs/developer/{packaging-install,api-contracts}.md`。

### USB 一键配网体验全面补强：写后报到闭环、有线与多网卡选址、串口自动刷新、纯 WPA3 预警与会话复用（2026-11-22）

- **现象**：USB 配网写完仅提示“几秒后报到”且写前不检查内置语音服务是否已启动，用户难判断板子有没有连上；双网卡（有线 + Wi-Fi）电脑上 `lan_ipv4_addrs` 此前把 Wi-Fi 排在有线前面，导致默认推荐易变的动态无线 IP，界面也写死“Wi-Fi IP”且未列出其余网卡地址；插拔数据线需手点刷新，读写握手期缺少阶段反馈；Windows 扫描附近网络固定硬等 3.5 秒且不区分纯 WPA3，多台板子连续配同一 Wi-Fi 需重复输入密码。
- **根因与修复**：① 选址与多网卡：`moss_voice_server::ota::lan_ipv4_addrs` 优先选系统默认出网真实网卡，代理 TUN 接管默认路由时改为有线网口（`以太网`/`Ethernet`/`eth*`/`enp*`）优先于无线；服务页改称“本机局域网 IP”，USB 配网高级选项按 `mossOtaQuickFillOptions` 列出全部真实局域网 IP 与 `moss.local`。② 写前写后闭环：`XiaozhiUsbProvisionCard` 在内置服务未运行时顶部提示并支持一键启动；写完指向本机 MOSS 的地址后自动启动 20 秒报到跟踪（`evaluateBoardCheckin`，合并写前后瞬时会话并兜底基线防旧连接/写盘时间戳误报，服务启动后才起算超时），实时区分上线 / 待允许 / 已撤销 / 超时排查。③ 串口与进度：未读板子时切回窗口静默刷新串口（无串口时每 4 秒后台探测一次，新插乐鑫原生口自动选中且不清空已预填草稿），读写期间展示分阶段进度提示。④ Wi-Fi 扫描与复用：`moss_usb_provision::wifi` 在 Windows 改用 `WlanRegisterNotification` 监听扫描完成提前返回（3.5 秒兜底），同步修正 `current_on` 传 `dot11BssType` 与 `bSecurityEnabled` 并跨平台识别纯 WPA3（macOS `CWSecurity`、Windows RSN IE Tag 48 AKM `2/4/6` vs `8/9/24`、Linux `nmcli`）提前标“仅 WPA3”预警；同进程内存保留上次成功写入的 Wi-Fi 供一键复用与手动清除，永不落盘。默认值、授权与安全门禁不变。
- **验证**：新增 `moss_voice_server::ota::tests::lan_ip_sorting_prefers_default_route_then_wired_over_wifi`、`moss_usb_provision::wifi::tests::{rsn_ie_distinguishes_wpa3_only_from_wpa2_and_transition,wpa3_only_parsing_and_merge_behavior}`，前端单测新增“多网卡快捷填入、纯 WPA3 预警、分阶段提示与写后报到闭环判断”与未启动渲染断言，浏览器用例补齐纯 WPA3 预警、多网卡填入、服务未启动一键启动、写后上线跟踪及内存复用与清除；`cargo test --workspace` 全工作区、前端 ESLint 零 error、前端单测 376/376、Vite 构建、`voice-center.browser.mjs`、脚本单测 58/58 与仓库分层/卫生检查全部通过。未在真实 Windows 无线网卡、真实小智 ESP32 与纯 WPA3 路由器上实机验收（`unverified`）。
- **关联**：`crates/moss-voice-server/src/ota.rs`、`crates/moss-usb-provision/src/wifi.rs`、`frontend/src/components/voice-builtin/{usbProvisionModel.js,XiaozhiUsbProvisionCard.jsx,BuiltinServicePanel.jsx}`、`frontend/src/styles/13-voice-devices.css`、`frontend/tests/{usbProvisionModal.test.mjs,voice-center.browser.mjs}`、`docs/developer/{api-contracts,frontend-behavior}.md`、`docs/user/quick-start.md`。

### 修复未配置模型（回声模式）下内置语音漏查小智自带工具，误报“固件不支持在线更新资源包”（2026-11-22）

- **现象**：新装机通过 USB 一键配网让小智（如 ESP32-S3 16MB、固件 2.5.0、v2 `assets` 分区表）连上内置语音服务后，若尚未填写对话模型密钥（服务运行在 `ServerMode::Echo` 回声模式），“设备 › 唤醒词”始终把设备标为“固件不支持在线更新资源包（需要小智固件 2.0.2 以上、v2 分区表）”，无法推送唤醒词与全屏画风，摄像头标签页也无法识别设备摄像头能力。
- **根因与修复**：`start_device_mcp` 与 `McpPending::Initialize` 此前仅在 `self.conversation.is_some()`（对话模式）时向设备发送 MCP `initialize` 与 `tools/list(withUserTools=true)`，回声模式下在线会话的 `device_tools` 恒为空、`self.screen.get_info` 也未调用，前端把空工具表误报为固件不支持。新增 `ServerConfig::eager_device_mcp`（桌面宿主默认开启）与幂等去重的 `DeviceRequest::SyncTools` / `VoiceServer::sync_device_tools`，使回声模式下 WebSocket 与 MQTT 常连设备同样自动完成工具清单与彩屏分辨率发现，连带打通回声模式按需摄像头抓拍的工具列表延迟排队，并在 MCP 握手/拉取超时或常连 `hello` 上报 `!features.mcp` 时自动收口加载态并释放滞留请求；`SessionView` 新增 `device_mcp_enabled` / `device_tools_ready`，`voice_wakeword_status` 新增 `tools_loading`，同步完成前界面显示“正在读取设备能力…”而不再误报固件不支持。
- **验证**：新增 `echo_mode_with_eager_device_mcp_discovers_tools_and_screen_size_over_mqtt` 与 `sync_tools_deduplicates_before_init_and_hello_without_mcp_clears_loading_state` 覆盖回声模式 MQTT 常连自动发现 `self.assets.set_download_url`、`320×240` 分辨率、下发资源包重启、队列去重及无 MCP 收口，`voice-center.browser.mjs` 补充读取中状态渲染断言；全工作区 Rust 测试、前端单测、17 项浏览器交互测试与构建通过，已同步 Mac `/Applications/MOSS.app` 与远端 Windows 正式版 `moss-desktop.exe`。未在真实小智硬件上点击确认推送重刷资源分区（`unverified`）。
- **关联**：`crates/moss-voice-server/src/{lib,state,session,session/connect,session/device,session/input,session/lifecycle,tests,tests/chat/mqtt}.rs`、`crates/moss-voice-host/src/builtin/{lifecycle,wakeword}.rs`、`frontend/src/components/VoiceWakeWordCard.jsx`、`frontend/tests/voice-center.browser.mjs`、`docs/developer/api-contracts.md`。

### 内置语音与小智控制台提示词收口：补齐对话层材料澄清、走边框中途控制并精简规则文档（2026-11-22）

- **现象**：内置语音底层安全铁律（`IMMUTABLE_SAFETY_RULES`）与 `DEFAULT_SYSTEM_PROMPT`、`PLAIN_HEAD` 存在重复字面量维护，且未在对话层提示词写明 PVC 拒刻、塑料/软卡成分澄清与预览后实物核对（仅靠调用工具触发后端报错）；官方小智控制台提示词误含内置语音专属 `play_music` 段落与已禁用的 `M3-5/改GRBL` 表述，占满 2000 字预算导致无法写入走边框途中的暂停/停止说明；`AGENTS.md` 与开发文档残留格式化指令冲突及已删除链路的旧快照。
- **根因与修复**：`moss-voice-server` 将 `DEFAULT_SYSTEM_PROMPT` 与 `PLAIN_HEAD` 收口为单源宏拼装，并在 `IMMUTABLE_SAFETY_RULES` 补齐 PVC/含氯材料拒刻、塑料/软卡先问材质及生成预览后提醒核对实物材料与厚度（对全部内置与自定义角色生效，后端门禁不变）；`xiaozhi-prompts/latest.md` 移除 `play_music` 与 `M3-5/改GRBL` 旧词并新增日期快照，补齐走边框中途 `pause` / `resume`（须确认）/ `stop_framing` 路由（1958/2000 字）；同步精简 `AGENTS.md`（统一 `verify.mjs --fmt-write`、下沉隐私细则、补齐触发路径与 macOS 打包命令）及 `safety.md`、`architecture.md`、`packaging-install.md`、`quick-start.md`、`docs/README.md`。
- **验证**：`guide.rs` 补充系统提示词材料澄清与实物核对断言，`persona` 与 `local_runtime_tests` 角色铁律回归通过，`node scripts/verify.mjs` 通过，已执行 `bash scripts/quick-update-desktop.sh` 同步本地正式版 `/Applications/MOSS.app`。未连接真实小智设备与激光机实测对话（`unverified`）。
- **关联**：`crates/moss-voice-server/src/{conversation,persona}.rs`、`crates/moss-voice-server/src/tests/chat/guide.rs`、`docs/integration/xiaozhi-prompts/{latest.md,history/2026-11-22-framing-controls-trim.md}`、`AGENTS.md`、`docs/README.md`、`docs/developer/{safety,architecture,packaging-install}.md`、`docs/user/quick-start.md`。

### 完整过程记录改为按先后的统一时间线，补齐加工全流程与语音细节（2026-11-22）

- **现象**：完整过程记录只覆盖语音每轮对话和激光生成预览，加工本身（连接、确认发送、急停、进度、结束）、审批、接入点调用、拾音与逐句播放、后台记忆整理和看图都没记；语音与激光分在两个文件里，每条也没有说明这一步在干什么。
- **根因与修复**：`ProcessTrace::record` / `save_file` 改为经独立后台单写线程非阻塞落盘（绝不阻塞急停 `0x18` 或异步音频协程，严格保证递增序号 `seq` 顺序、单行 `timeline.txt` 换行清理、附件与清空串行化及文本硬上限），输出 `timeline.jsonl` 与可读 `timeline.txt`。激光侧在统一入口记每次改动类接口调用（`api_call`，发起方、做什么、请求、结果与耗时，过滤只读轮询含缺省 `action` 的 `/api/workflow` 与 `/api/material-lab/{status,recent}`、凭据接口及 >256 KiB 大报文体，先递归脱敏凭据/URL/内嵌 `sk-`/`Bearer`/数值 PIN 再拼 `step` 摘要）；加工线程经 `Condvar` 守卫按实时 `lines_sent` 记开始、每 10% 进度与结束（不晚于 `job_end` 且不漏线程）。语音侧补记拾音、开口、Jev 筛选、大模型轮次编号、逐句播放、打断、被下一轮接替与常连断开时的收尾、后台记忆整理的大模型调用与看图（照片仅在设备与视觉开关复核通过后落盘），按 `ActiveTurn.turn_number` 固定每轮 `flow`，并在偏好设置确认文案中明示包含设备拍照看图的照片。顺手将 `laser-mcp` 开关测试 `SwitchBackend` 改为独立遥测实例，消除与并行测试争用全局计数器的偶发失败。开关、默认关闭、保留 7 天与权限均不变。
- **验证**：新增 / 更新测试覆盖时间线后台单写顺序与 `flow` 换行防断、激光脱敏（含 `step`/URL/`Bearer`/`sk-`/数值 PIN/只读 POST 过滤）、加工开始/10% 进度节流/结束先后顺序、语音一轮完整步骤与第 1 轮 `flow` 归属、`TracingLlm` `step` 脱敏、`laser-mcp` 隔离遥测及前端看图照片提示；`node scripts/verify.mjs --e2e`（静态门禁、脚本、全工作区 Rust、前端单测/构建/浏览器交互与回环 E2E）全通过，已同步本地正式版 `/Applications/MOSS.app`。未连真实激光机与小智设备（`unverified`）。
- **关联**：`crates/laser-runtime/src/process_trace.rs`、`crates/laser-api/src/{process_trace.rs,routes.rs,send.rs,feedback.rs}`、`crates/laser-mcp/src/tests.rs`、`crates/moss-voice-server/src/{session.rs,session/trace.rs,session/{input,listen,playback,turn,lifecycle}.rs,state.rs,conversation.rs,vision.rs}`、`frontend/src/{processTrace.js,components/ProcessTraceSection.jsx}`、`docs/developer/{api-contracts,frontend-behavior,feature-glossary}.md`、`docs/user/quick-start.md`、技术书 n106、融合技术书 r56。

### 新增“完整过程记录”：语音对话与激光生成的全过程可保存在本机（默认关闭，2026-11-22）

- **现象**：事后想核对“当时说了什么、模型怎么回的、生成了什么”时，只有最近 500 条脱敏对话摘要；诊断事件、耗时只在内存里，退出 MOSS 就没了；发给大模型的完整内容、模型原始回复、录音与合成语音从不保存。
- **根因与修复**：新增共用记录器 `laser_runtime::ProcessTrace` 与偏好设置“完整过程记录”开关（`settings.json` 的 `process_trace_enabled`，**默认关闭**，关着时什么都不写、行为不变；打开前确认一次，改了立即生效）。打开后写入运行目录 `logs/process-trace/<日期>/`：语音每轮的用户录音 WAV、识别原文与纠正后、每轮大模型完整请求与回复、工具完整参数与结果、播出的句子与合成语音 WAV、耗时与诊断事件（新增 `TurnEvent::Trace`，会话层统一带上设备 / 会话 / 轮次）；激光每次生成的参数、发起方、产物与 SHA-256（G-code、预览图复制一份）、生成失败原因、加工反馈的 Jev 请求与原始返回。写入前去掉密钥与令牌，不上传；保留 7 天、总量上限 2 GiB，可一键清空。新增 `POST /api/process-trace {action:"status"|"clear"}`，与 `/api/settings` 同权限，语音、接入点与局域网一律拒绝。
- **验证**：新增 Rust 测试：记录器 4 项（关着不写、写事件与附件、开关即时生效、7 天与总量清理）、激光 3 项（生成记录与产物复制、带路径文件名不复制、接口权限与清空）、语音 3 项（端到端一轮依次记下录音 / 识别 / 两轮大模型 / 工具结果 / 轮次结束含合成语音 / 耗时；关着时不建目录；凭据去除与图片省略）；前端单测 2 项与设置页浏览器用例（默认关、取消不保存、确认后打开、占用显示、清空、关闭）。未用真实小智设备与真实模型听验录音和合成语音文件（`unverified`）。
- **关联**：`crates/laser-runtime/src/process_trace.rs`、`crates/laser-api/src/{process_trace.rs,routes/system.rs,routes/workflow.rs,feedback.rs,access.rs,service.rs}`、`crates/laser-host/src/lib.rs`、`crates/moss-voice-server/src/{session/trace.rs,conversation.rs,state.rs,session/turn.rs,session/lifecycle.rs}`、`crates/moss-voice-host/src/builtin/{conversation.rs,lifecycle.rs}`、`frontend/src/components/{ProcessTraceSection.jsx,SettingsModal.jsx}`、`frontend/src/processTrace.js`、技术书 n105、融合技术书 r55、`docs/developer/{api-contracts,frontend-behavior,db-schema,feature-glossary}.md`、`docs/user/quick-start.md`。

### USB 一键配网读出的旧 OTA 地址会提醒并可一键换成 MOSS 当前地址（2026-11-22）

- **现象**：读取小智板子后，“自定义 OTA 地址”显示板子里存的历史值，不再跟随 MOSS 当前地址；电脑换了 IP 或改了内置服务端口时，确认条只写“写入 OTA 地址：旧地址”，写完板子连不上 MOSS。
- **根因与修复**：读取后直接沿用板子原值，没有与 MOSS 当前地址比对。新增 `staleBoardOta` / `mossOtaUrls`：板子原值不在 `ota_ip_url` / `ota_url` / `ota_ip_urls` 之中（协议与主机不分大小写、末尾斜杠不计）且输入框仍是原值时，在 Wi-Fi 标签与高级选项提醒并给“换成 MOSS 当前地址”；默认不替换（可能故意指向别的服务器）。
- **验证**：新增单测“读出的板子 OTA 地址不是 MOSS 当前地址时提醒，默认不替换”（换 IP、改端口、多网卡、大小写与末尾斜杠、已改掉 / 没存过 / 服务无地址），浏览器用例覆盖旧地址提醒、默认保留与一键替换、地址一致时不提醒。
- **关联**：`frontend/src/components/voice-builtin/{XiaozhiUsbProvisionCard.jsx,usbProvisionModel.js}`、`frontend/src/styles/13-voice-devices.css`、`frontend/tests/{usbProvisionModal.test.mjs,voice-center.browser.mjs}`、技术书 n104。

### 首次标定向导的每一步都能单独收起（2026-11-22）

- **现象**：材料实验室“首次标定向导”里，点当前步骤的标题收不起来，只能把整个向导折叠。
- **根因与修复**：收起时展开状态被设为空，组件又回落到“当前步骤”，同一步立刻重新展开。展开状态改为“跟随当前步骤 / 全部收起 / 用户点开的那一步”三种（`resolveOpenWizardStep`、`toggleWizardStep`），完成或跳过一步后恢复跟随；标题加展开箭头与 `aria-expanded` / `aria-controls`。
- **验证**：新增单测“标定向导每一步都能单独收起……”与面板渲染断言（六个可展开标题、仅当前步骤展开），`calibrationWizard.test.mjs` 8 项通过；材料实验室没有浏览器交互用例，未做浏览器点击验证。
- **关联**：`frontend/src/components/CalibrationWizard.jsx`、`frontend/src/calibrationWizard.js`、`frontend/src/styles/15-addenda-2026-11.css`、`frontend/tests/calibrationWizard.test.mjs`、融合技术书 r54。

### USB 一键配网可一键填入电脑当前 Wi-Fi 或扫描附近网络（2026-11-22）

- **现象**：USB 一键配网的“新的 Wi-Fi”只能手动输入 SSID，容易输错名字，也看不出路由器是不是板子连不上的 5 GHz。
- **根因与修复**：新增 `moss_usb_provision::wifi` 与 `voice_usb_provision` 的 `wifi_current` / `wifi_scan` 两个只读动作（不碰串口、不占配网锁、不写诊断、从不读取密码）：macOS 用 CoreWLAN 并在主线程申请定位授权（`Info.plist` 加 `NSLocation*UsageDescription`），Windows 用 wlanapi，Linux 用 `nmcli`；同名多 AP 合并信号与频段。界面在 SSID 下加“用本机当前 Wi-Fi”“扫描附近”，扫描列表按读到的芯片只显示板子能连的频段（非 ESP32-C5 只显示 2.4 GHz，C5 显示 2.4 / 5 GHz，连不上的隐藏并注明个数，双频网络信号按对应频段的 `band_signals` 算），标加密与信号，表头可“重新扫描”；换网络名会清空已填密码。缺定位权限或电脑 Wi-Fi 开关关着时（新错误码 `wifi_radio_off`），macOS / Windows 弹应用内选择框“去系统设置打开 / 手动输入”（USB 配网卡片“全程无弹窗”的唯一例外）：前者经新动作 `open_wifi_settings` 只跳固定的定位权限页或 Wi-Fi 设置页，切回 MOSS 时自动重查；后者聚焦 SSID 输入框，提示行保留“去系统设置打开 / 重新检查”供之后改主意。`scripts/quick-update-desktop.sh` 同步把源码 `Info.plist` 新增的顶层键带进已安装的 `.app`（否则 macOS 静默拒绝定位请求）。Android 不编译 USB 配网，不适用。
- **验证**：新增 Rust 单测 9 项（频段、信号换算、合并去重、`nmcli` 转义解析、提示与序列化、设置页地址固定）及请求解析断言，前端单测“本机 Wi-Fi：按芯片判断频段…”与渲染断言，浏览器用例覆盖缺权限弹窗选手动输入、之后从提示去设置打开并切回自动重查、填入当前 Wi-Fi、5 GHz 提示、按芯片筛选、点选与重新扫描；`cargo test --workspace` 1,463 项、前端 370 项、浏览器 17 项全部通过，clippy 零警告，Windows / Linux 目标单独交叉 clippy 通过。本机定位服务整体关闭，macOS 授权弹框与真实扫描、Windows 实机扫描均未实测（`unverified`）。
- **关联**：`crates/moss-usb-provision/src/{wifi,lib}.rs`（`wifi_query`、`UsbProvisionRequest::{WifiCurrent,WifiScan,OpenWifiSettings}`、`wifi::open_settings`）、`scripts/quick-update-desktop.sh`、`crates/moss-voice-host/src/{lib.rs,builtin/usb_provision.rs}`、`apps/desktop/src-tauri/{Info.plist,src/voice_builtin.rs}`、`frontend/src/components/voice-builtin/{XiaozhiUsbProvisionCard.jsx,usbProvisionModel.js}`、`frontend/src/styles/13-voice-devices.css`、`docs/developer/api-contracts.md`、技术书 n103。

### 修复第三批重构遗留：打包脚本旧 db.rs 路径、局域网扫描绕过开连接锁与残留命令表（2026-11-21）

- **现象**：拆分 `db.rs` 后，`scripts/build-macos-installer.sh`（`--with-voice-sidecar`）与 `scripts/voice-helper/build-sidecar-installer.ps1` 校验 schema 版本时仍读已删除的 `crates/laser-runtime/src/db.rs`，打包直接报错中断；局域网设备扫描的确认线程开 Telnet 连接前没拿 `target_open_mutex`、没查也没登记短连接占用，可能与并发启动的发送或连接探测双开同一端口，且候选键按 `ip:port` 拼接，IPv6 目标匹配不到正在加工的任务；`safe_action` 里还留着一份 38 项实时命令名单；`moss_voice::settings::update_root` 已无生产调用方；文档里材料下拉、审批卡层级与 `voice_devices` 换 `Client-Id` 的说明落后于 2026-11-20 的改动。
- **根因与修复**：两个打包脚本改读 `db/mod.rs`；局域网扫描的工作线程与降级串行分支统一走 `probe_discovery_candidate`：持 `target_open_mutex` 依次复查活动任务、待确认任务与短连接占用，空闲才开连接并登记 `ShortIoGuard`，随即释放打开锁再问询，结束时先放本地句柄再释放占用；候选键改用 `ConnectionSpec::describe()`（IPv6 带方括号）。`safe_action` 的互斥判定改为直接用 `laser_tools::realtime_action` 的解析结果；删除 `update_root`，其并发测试改调 `settings_store::update`。同步 `api-contracts.md`（`material-options` 的 `surface` 字段与纯表面材料可选）、`frontend-behavior.md`（`.approval-center-modal` 128）、`db-schema.md`（代码路径、待允许 64 条上限、换 `Client-Id` 不降级）、`moss-native-voice-spec.md` 允许流程与 `db/voice.rs` 注释。
- **验证**：新增 `discovery_probe_skips_target_held_by_short_io_including_ipv6`（被占用目标返回 `probing` 且不开连接，IPv6 按方括号命中）与 `discovery_probe_registers_and_releases_short_io_for_idle_target`（问询期间登记、结束释放），新增脚本护栏“打包与发版脚本里写死的 Rust 源码路径都存在”（还原旧路径时失败）；`cargo clippy --workspace --all-targets -- -D warnings` 零警告，`cargo test --workspace` 1,455 项、58 项脚本单测、分层与卫生门禁、回环 E2E 全部通过；已执行 `bash scripts/quick-update-desktop.sh --with-host`。两个打包脚本未实跑完整打包，局域网扫描未连真实雕刻机（`unverified`）。
- **关联**：`scripts/build-macos-installer.sh`、`scripts/voice-helper/build-sidecar-installer.ps1`、`scripts/tests/release-gates.test.mjs`、`crates/laser-api/src/{connection,safe_action}.rs`（`probe_discovery_candidate`、`DiscoveryBusyTables`）、`crates/laser-api/src/tests/stop_and_resume.rs`、`crates/moss-voice/src/settings.rs`、`crates/laser-runtime/src/db/voice.rs`、`docs/developer/{api-contracts,frontend-behavior,db-schema}.md`、`docs/integration/moss-native-voice-spec.md`。

### 第三批重构 3B：前端设备连接存储收口与发送“结果未知”状态转移抽成纯函数（2026-11-21）

- **现象**：同一组设备连接存储键在工作台、材料实验室、数字孪生、画板默认表单、AI 预览与连接栏 6 个文件里各自手写读写；工作台开刻 / 续雕“结果未知”的处理复制了 4 份，材料实验室的发送与轮询状态转移写在页面里，没有单测。
- **根因与修复**：新增 `deviceConnection.js`（工作台 `laser-forge-*` 与画板 `laserLab.*` 连接键唯一登记处）与 `useDeviceConnection`（工作台、材料实验室共用，只写回变化的字段），6 处改用它；行为变更：材料实验室自动选中的串口现在也会记住。抽出 `matrixSendOutcome` / `matrixPollOutcome`（材料实验室）与 `workbenchConnectionFields` / `workbenchCanonicalTarget` / `sendProgressForUnknownResult`（工作台），“结果未知继续轮询、保留规范急停目标、不报失败”的语义不变。`DrawPage` 持久化逻辑与十余个页面状态交织、纯逻辑已在 `drawScenePersist.js`，评估后不再抽 Hook。
- **验证**：前端 ESLint 零告警，369 项单测（新增 `deviceConnection.test.mjs` 4 项、矩阵发送 / 轮询与工作台连接 3 项；`targetedFixes` 源码护栏改指向新的纯函数位置，未放宽）与生产构建通过；回环 E2E 40/40 通过（含画板继承工作台设备、材料实验室矩阵下发与双设备切页急停）；浏览器交互测试 16/17，`twin-render.browser.mjs` 在本机无头软件 WebGL 下 0.5 秒出帧 8 次低于阈值，恢复改动前的 `TwinPage.jsx` 后同样失败，与本次无关；已执行 `bash scripts/quick-update-desktop.sh --with-host`。
- **关联**：`frontend/src/{deviceConnection.js,useDeviceConnection.js,materialLabState.js,workbenchParams.js,aiPreviewState.js,drawConnection.js,pages/{HomePage,MaterialLabPage,TwinPage}.jsx,components/material-lab/LabConnectionBar.jsx}`、`docs/developer/frontend-behavior.md`。

### 第三批重构 3C：按域拆分 RuntimeDb 与 laser-mcp、合并 GRBL 实时命令表（2026-11-21）

- **现象**：`laser-runtime/src/db.rs`（3,042 行）与 `laser-mcp/src/lib.rs`（3,444 行）单文件过大；GRBL 实时字节命令表（急停 / 软复位、进给与功率倍率）在 `laser-api` 与 `laser-tools` 各抄一份。
- **根因与修复**：`RuntimeDb` 按域拆成 `db/{mod,migrate,materials,approvals,voice,secrets,maintenance}.rs`，`laser-mcp` 拆成 `lib`、`schema`、`dispatch`、`rpc`、`bridge` 与 `tests`，代码原样搬移，对外接口、schema v20 与行为不变；实时命令表收口为 `laser_tools::realtime_action`，`laser-api` 后台安全动作复用。行为变更：后台软复位的回执文字由“急停 0x18”改为与直连一致的“软复位 0x18”，字节不变。内置工具提示词模板里的代码路径同步改为 `laser-mcp/src/schema.rs`、`dispatch.rs`。评估后不做：`RECORD_TRANSITION_LOCK` 保持进程级（发送线程里的自由函数持有，改实例字段要穿过 6 处以上调用点，漏一处会重现“永远发送中”的竞态，持锁仅微秒级）；`InProcessBackend` / `VoiceBackend` 合并会改到内置语音 R4 拒绝门禁，收益约 40 行，暂缓。
- **验证**：`laser-runtime` 86 项、`laser-mcp` 29 项测试拆分前后一致，公开方法数一致；`cargo clippy --workspace --all-targets -- -D warnings` 零警告，`cargo test --workspace` 1,453 项通过；真实激光设备未验（`unverified`）。
- **关联**：`crates/laser-runtime/src/db/`、`crates/laser-mcp/src/`、`crates/laser-tools/src/lib.rs`（`realtime_action`）、`crates/laser-api/src/safe_action.rs`、`frontend/src/voiceState/settings.js`、`docs/developer/{architecture,db-schema,frontend-behavior}.md`。

### 第三批重构 3A：settings.json 统一读写入口，修复 Android 导出设置绕过写锁（2026-11-21）

- **现象**：`settings.json` 在 `laser-api`、`moss-voice`、`laser-host`、`laser-runtime` 与桌面壳各有一套读写；Android 的导出设置直接裸写、绕过进程内写锁，可能与 `/api/settings` 并发保存时互相覆盖；全局设置锁放在第 0 层 `laser-contracts`；Jev 决策接口默认地址在三个包里各写一份。
- **根因与修复**：新增 `laser_runtime::settings_store`（`update` / `lock` / `read` / `read_checked`）：进程内唯一写锁 + 同目录临时文件原子替换，非空但不是 JSON 对象的文件拒绝覆盖，修改闭包出错不写盘；`laser-api`、`moss-voice::settings`（7 处）、`laser-host`、`LaserSettings` 与桌面壳 `export.rs`（含 Android 分支）全部改用它，删除 `laser_contracts::with_settings_file_lock`。`moss-voice` 与桌面壳新增对第 1 层 `laser-runtime` 的依赖；`moss-voice` 写设置失败的提示与 `laser-api` 统一为“读取 / 写入 settings.json 失败：…”。Jev 地址收口为 `laser_contracts::OPENROUTER_DECISIONS_URL`，独立的 `moss-voice-server` 保留自有常量并加测试与 `moss-voice` 比对。评估后不拆 `moss-voice-config`（`settings.rs` 依赖 `colab`、`local_models`、`models`，调用方本就同链进桌面程序，编译收益小）。
- **验证**：新增 `settings_store` 5 项（保留其他键无残留临时文件、损坏 / 非对象文件不覆盖、缺失文件与父目录创建、闭包失败不写、16 线程并发写不同键全部保留）与 `defaults_match_voice_center` 1 项；`moss-voice` 96 项通过；全工作区 clippy 零警告、1,453 项测试通过，分层门禁通过；Android 实机导出设置未验（`unverified`）。
- **关联**：`crates/laser-runtime/src/{settings_store.rs,config.rs,lib.rs}`、`crates/laser-api/src/settings.rs`、`crates/moss-voice/{Cargo.toml,src/settings.rs,src/models.rs}`、`crates/laser-host/src/lib.rs`、`crates/laser-contracts/src/lib.rs`、`crates/moss-voice-server/src/jev.rs`、`apps/desktop/src-tauri/{Cargo.toml,src/export.rs}`、`docs/developer/architecture.md`。

### 加固缓存清理脚本安全边界、修复发版重跑正文哈希失配并同步开发文档（2026-11-21）

- **现象**：`scripts/clean-build-cache.sh` 漏扫 `.dylib` OSO 调试引用、macOS 缺 `nm` 时 fail-open 删文件、非 UTF-8 符号名可能触发 `UnicodeDecodeError`、`--dry-run` 对超期增量会话重复计费、未阻断中间段软链穿透，且与 `clean-build-cache.ps1` 一并将独立工作区 `apps/moss-voice-local/target` 当成误生子目录整删；`release.yml` 重跑已有 Release 覆盖上传资产后未更新正文 SHA-256 表；部分开发文档滞后于近期重构（缺少 3 个新 crate、局域网配对 token 持久化与协议 7 / `CATALOG` 27 项 / `kws` / `dispatch_json` / 可注入遥测 / schema v20 `status_revision`）。
- **根因与修复**：`clean-build-cache.sh` 补齐 `profile_dir`/`deps_dir` `.dylib` OSO 扫描、macOS 缺 `nm` fail-closed、`errors="replace"`、已标删父目录去重、`has_symlink_component` 全链路软链拦截（含第 0 步 `cargo-sweep`），并与 `clean-build-cache.ps1` 同步豁免 `apps/moss-voice-local/target` 整删（改由 `cargo-sweep` 按 3 天清理）；`clean-build-cache.ps1` 同步修复 `Get-Item` 目录匹配、不跟随重解析点的目录遍历与 `Remove-TreeSafe`（清只读并容错占用锁文件）；`release.yml` 在 `gh release upload --clobber` 后追加 `gh release edit "$TAG" --notes-file release-notes.md`；同步 `AGENTS.md` 与 `docs/developer/{standards,architecture,api-contracts,db-schema}.md`。
- **验证**：`scripts/tests/clean-build-cache.test.mjs` 新增 2 项回归测试（覆盖 `--dry-run` 不重复计费、叶子/中间段软链防穿透、豁免 `apps/moss-voice-local/target`、macOS `.dylib` OSO 与缺 `nm` fail-closed），`scripts/tests/release-gates.test.mjs` 补齐重跑正文同步断言；57/57 项脚本单测、`bash scripts/clean-build-cache.sh --dry-run`、`check-crate-layers.mjs` 与 `check-repo-hygiene.mjs`（含 `.ps1` 纯 ASCII）全通过。未改动桌面/后端源码，跳过桌面主程序重编；`clean-build-cache.ps1` 与 `release.yml` 实跑待 Windows 与发版流水线验证（`unverified`）。
- **关联**：`scripts/clean-build-cache.sh`、`scripts/clean-build-cache.ps1`、`scripts/tests/clean-build-cache.test.mjs`、`.github/workflows/release.yml`、`scripts/tests/release-gates.test.mjs`、`AGENTS.md`、`docs/developer/{standards,architecture,api-contracts,db-schema}.md`。

### 修复激光安全与并发、语音权限边界及前端急停状态高危隐患（2026-11-20）

- **现象**：激光发送收尾阶段报警/断线与取消并发时故障会被吞并，并发 `resume` 存在绕过非取消态的竞态，连接探测与非变更安全动作未隔离并发双开且探测会剥离 `UncertainJob` 句柄，图片预览与 `S 500`/`S+500` 功率缩放、`$CD` 异常、SD 哈希校验、局域网扫描线程和停机终态存在漏洞；正式版默认放行开发端口 CORS，`/v1/auth/challenge` 漏校桌面会话，内置语音服务缺少连接/待审设备/固件下载并发上限、OTA 错绑 `Client-Id` 仍签发令牌、`voice_persona_tool` 撤销后仍可写回，`moss-mcp-client` 未约束子进程树与 HTTP 代理；前端发送/续雕断连（`status === 0`）误报失败或急停串口目标失配、`WorkbenchCanvas` 拖拽可能残留全局点击捕获器吞掉顶栏急停，以及局域网预览 401、审批窗被预览窗遮挡、纯表面材料选项缺失、弹窗关闭未清敏感状态和多处轮询/吞错问题。
- **根因与修复**：
  - 激光链路与接口门禁：`laser-device` 新增 `wait_until_idle_or_cancel`、修复流式写入与收尾取消并存时的故障优先权、`verify_sd_file_hash` 与 `cancel_with_sd_monitoring` 的 EOF/未终止/复位检测、`rescale_gcode_line_s` 空白与 `+` 号处理，以及 `fetch_fluidnc_config`/`load_features`/`probe_opened_device` 仅豁免旧 GRBL `error:3`（`laser_off` 跳过 `$CD`，`unlock` 在非断线/非失联的 Alarm 态仍可解锁）；`laser-api` 统一 `target_open_mutex -> active_jobs -> uncertain_jobs -> short_io_targets` 锁序并引入 `ShortIoGuard` 防双开且允许 `alters_alarm` 急停直接复用在途句柄并递增 `cancel_epoch`，收紧 `validate_resume_record` 在预留锁后仅放行 `cancelled`，移除图片预览路径穿越兜底，补齐 `safe_action` 失败迁移 `uncertain_jobs`、`finalize_workflow_record` 保护 `result_unknown`、设备发现单飞与有界线程池，并将开发端口 CORS 限定为 debug 构建、`/v1/auth/challenge` 在桌面会话启用时强制校验 `X-Moss-Desktop-Session`。
  - 语音与第三方 MCP 权限边界：`moss-voice-server`/`laser-runtime`/`moss-voice-host` 补齐 HTTP/WS/MQTT 连接与固件分块流式下载并发限额、64 条待审设备上限、O(1) 设备授权查询、OTA 按 `bound` 签发令牌与方括号 IPv6 `Host` 校验，以及 `PersonaStore::save_active_if` 锁内撤销复核；`moss-mcp-client` 接入 `process-wrap`（`KillOnDrop` + Windows `CREATE_NO_WINDOW`/`JobObject` + Unix `ProcessGroup::leader` 与 `waitid(WNOWAIT)` 退前进程组清扫）并启用 `.no_proxy()` 与 `http://localhost` 回环固定解析。
  - 前端安全与契约：`HomePage`/`MaterialLabPage`/`AiPreviewCenter` 断连未知结果统一保留规范目标并持续轮询，`WorkbenchCanvas` 自动撤除点击捕获器并清理 `pointercancel`/卸载监听，统一局域网鉴权预览与 `ApprovalCenter` 层级，补全 `DrawMaterialCard`/`WorkbenchMaterialCard`/`MaterialModal` 纯表面材料支持、`HomePage` 握手期防重入、`SettingsModal`/`CloseConfirmModal` 关闭重置、`TwinPage` 与 5 个语音卡片单调序号/单飞守卫，以及 4 处吞错与乐观更新回滚。同步更新 `docs/developer/api-contracts.md` 与 `docs/developer/safety.md`。
- **验证**：`cargo clippy --workspace --all-targets -- -D warnings` 零警告，`cargo test --workspace` 1,447 项全通过（51 组，含新增并发/安全/进程树回归用例），`frontend` ESLint 零告警、362 项前端单测与 Vite 生产构建通过，55 项脚本单测与分层/卫生门禁通过，40/40 项回环 E2E（API + Chrome UI）通过，16/17 项浏览器交互测试通过（仅 `tests/twin-render.browser.mjs` 在无头软件 WebGL 下 0.5 秒出帧 10 次略低于既有阈值 11），已执行 `bash scripts/quick-update-desktop.sh --with-host` 原地更新并验签本机 `/Applications/MOSS.app` 与 `moss-host` 并清理 3 天超期缓存。独立工作区 `apps/moss-voice-local` 未改动未重测；真实激光硬件、真机小智固件与 Windows/Android 实机打包未验（`unverified`）。
- **关联**：`crates/laser-device/src/{connection,connection/job,probe,discovery}.rs`、`crates/laser-tools/src/lib.rs`、`crates/laser-api/src/{access,connection,jobs,routes,safe_action,send,workflow,generation/image_upload}.rs`、`crates/laser-runtime/src/db.rs`、`crates/moss-voice-server/src/{lib,firmware,mqtt,ota,persona,session/connect}.rs`、`crates/moss-voice-host/src/builtin/{conversation,devices,plugins}.rs`、`crates/moss-mcp-client/{Cargo.toml,src/client.rs}`、`frontend/src/{pages,components,styles/10-component-inline.css,authSession.js,materialSelectState.js}`、`docs/developer/{api-contracts,safety}.md`。

### `clean-build-cache.sh` 补齐 macOS 孤儿与失效 `.rcgu.o` 自动清理（2026-11-20）

- **现象**：macOS 默认使用 `split-debuginfo=unpacked`，在 `target/*/deps/` 下留下大量 `<crate>-<hash>.<cgu>.rcgu.o` 及无哈希静态库 `.rcgu.o`；`cargo-sweep` 与原 `clean-build-cache.sh` 按完整后缀分桶均无法识别，长期堆积可达十余 GB。
- **根因与修复**：在 `scripts/clean-build-cache.sh` 追踪本趟待删/已删路径并新增第 6 步，按 `(stem, hash)` 存活产物、Mach-O 顶层二进制 `nm -pa` OSO 调试表以及 `ar -t` 静态库归档成员交叉核验，在同趟清理及 `--dry-run` 预览中统一剔除孤儿与失效 `.rcgu.o`，并按 `deps/` 汇总输出避免刷屏。
- **验证**：新增 `scripts/tests/clean-build-cache.test.mjs` 2 项真实临时目录回归测试（覆盖 `--dry-run` 同趟联动、存活第三方与带连字符二进制保留、真实 `ar` 归档成员校验及 3 天超期孤儿清理），`bash scripts/clean-build-cache.sh --dry-run` 与 `node scripts/verify.mjs` 全部通过；未改动桌面程序源码，跳过桌面主程序重编。
- **关联**：`scripts/clean-build-cache.sh`、`scripts/tests/clean-build-cache.test.mjs`。

### 固定 Android 验签工具版本，修复正式证书被误判不匹配（2026-11-19）

- **现象**：`v0.3.12` 已完成 Android APK 构建，但验签步骤报证书不匹配，发布任务被跳过。
- **根因与修复**：Build Tools 37.0.0 改变了 `apksigner --print-certs` 的输出，原流程自动选最新版后读不到指纹。发版验签固定到 36.0.0，比对失败时打印两个证书指纹，仍要求 APK 仅有一个与固定证书吻合的签名者。
- **验证**：用两版 `apksigner` 验证同一份已发布的 `v0.3.11` APK，确认指纹与固定证书一致且旧解析在 37.0.0 下读不到指纹。本机构建的 `v0.3.12` APK 通过验签及更新签名校验，53 项脚本测试通过，GitHub 三端构建与发布成功。
- **关联**：`.github/workflows/release.yml`、`scripts/tests/release-gates.test.mjs`、`docs/developer/packaging-install.md` §3.8。
