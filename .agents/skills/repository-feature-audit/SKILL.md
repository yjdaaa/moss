---
name: repository-feature-audit
description: Use when a user asks what features a repository currently supports and how their implementation behaves.
tags: ["自习"]
---

# 仓库功能现状审查

通过只读检查，把用户关心的功能映射到实际入口、状态流转和具体实现，并给出可追溯的结论。本技能不把设计文档中的计划视为已交付能力，也不以阅读代码代替运行测试或设备实测。

## When to Use

适用于“现在有哪些功能”“怎么识别”“有哪些保障措施”“打断后怎么重新开始”等要求解释当前仓库行为的问题，尤其是文档同时包含规划和实施记录、功能横跨多个模块时。

Don't use：用户要求直接改代码；只需概述用户已提供的文本；问题完全不涉及仓库现状。

## Procedure

1. 用 PI 的 `read` 查看相关文档，先标记每段是设计、实施记录还是测试结果。遇到明确写着“只定义拟实施方案，不代表当前能力”的文档，不得直接把功能对照表中的“采用”列当作现状。本次示例文档为 `docs/integration/moss-native-voice-spec.md`。
2. 用 `glob` 定位实际模块，再用 `grep` 搜索用户问题中的事件名、状态名和函数名；用 `read` 沿入口追踪处理分支。语音示例包括 `crates/moss-voice-server/src/session.rs`、`crates/moss-voice-server/src/conversation.rs`、`crates/moss-voice-server/src/audio/pump.rs`；搜索目标包括 `ClientMessage::Listen`、`ClientMessage::Abort`、`interrupt`。
3. 为每项能力记录证据级别：仅设计、代码存在、测试覆盖、实际运行已验证。跨模块能力要确认接线和启用条件；例如存在提供商接口，不等于用户已经配置模型或设备正在使用它。
4. 按事件顺序还原行为，避免把相似事件合并。此例中 `listen start` 在忙碌时调用 `interrupt` 后调用 `start_listening`；`abort` 在忙碌时调用 `interrupt`，但该分支没有紧接着调用 `start_listening`。唤醒回应期间的 `listen start` 则先记录 `listen_after`，不立即打断问候。
5. 检查边界条件和补救路径，而不只读注释：`on_binary` 仅在 `Phase::Listening` 接受音频；`finish_utterance` 区分空音频与有效语音；`interrupt` 在符合条件时保留识别原话和已送去播放的句子，播放阶段通过 `pump.clear()` 丢弃未播内容并发送 `TtsState::Stop`。
6. 用户问“我的设备固件支持吗”时，先区分仓库源码、烧录包、硬件清单和设备上实际运行的版本。可用 PI 的 `bash` 只读查看 `使用说明.txt`、烧录配置 `configure/esp32s3/spi_download.conf` 中的 `file_path0` 与 `file_offset0`，确认分析的是哪个镜像；再检查镜像中的 `AEC`、`AfeAudioProcessor`、`Device AEC is not supported`、`The playback reference channel is 0, the AEC is deactivated.` 等字符串，并对照组装 BOM 的麦克风与功放型号。字符串说明镜像包含相关代码或日志，不能单独证明该分支在设备上执行、AEC 初始化结果或播放时是否持续采集和上传音频；BOM 也不能单独证明不存在软件提供的播放参考信号。需要实际运行日志、固件源码/配置或播放时录音测试才能确认这些行为。
7. 用户提供开发板 Wiki 或固件下载页时，用 PI 的 `bash` 核对页面标题、适配板型和镜像版本，再从页面中的实际链接追到硬件文档及对应板级源码，不要猜章节 URL。本例页面 `https://wiki.lckfb.com/zh-hans/szpi-esp32s3/xiaozhi-ai.html` 指向实战派 ESP32-S3；实际音频输入章节为 `https://wiki.lckfb.com/zh-hans/szpi-esp32s3/beginner/audio-input-es7210.html`。该章节描述 ES7210 的 MIC3 接收 ES8311 输出作播放参考；`78/xiaozhi-esp32` 的 `main/boards/lckfb/szpi-esp32s3/config.h` 设置 `AUDIO_INPUT_REFERENCE true`，`lichuang_dev_board.cc` 将物理 MIC3 指为参考输入。还需追到 `main/audio/engines/afe_audio_engine.cc`：虽然按参考通道设置 `aec_init`，代码也会调用 `disable_aec`，并受唤醒检测和语音处理状态控制。此链条证明对应源码具备条件式 AEC 路径，不证明 Wiki 下载的 V1.4.6 镜像、OTA 后版本或用户设备实际启用了它。
7. 回答时按“已实现行为／依赖配置或设备配合／仅有设计或尚未核验”组织。性能数字、识别准确率、真实扬声器停播时间等，只有对应实测证据时才给出确定结论；否则明确说明代码阅读无法确认。

## Pitfalls

- 设计稿列出提供商、插件或安全措施，看起来像功能清单，却可能尚未实施；回到运行入口和实际接线确认。
- 代码支持 `listen start` 打断，不等于设备一定能靠用户自然插话发送该消息；设备侧唤醒、回声消除和上行行为需要另查。
- `abort` 停止当前回答不等于自动重新开麦；须分别检查各消息分支的后续状态转换。
- `handle.abort()` 中止任务不等于已经证明所有外部请求、计费或设备播放都瞬间停止；避免使用“瞬间静音”“不再消耗 Token”等未经验证的措辞。
- 预缓冲发送帧数与播放泵开始发送前积攒的音频量是两个机制，不能把前者直接表述成全部预缓冲时长。
- 把已送入播放队列的句子称为“用户确实听到的内容”会夸大证据；应按代码实际记录时点描述。
- 固件里存在 `Device AEC is not supported` 或 `The playback reference channel is 0, the AEC is deactivated.` 等日志模板，不等于当前设备实际打印过这些日志；不要据此断言启动时必然关闭 AEC。
- `HTTP 200`、下载成功或文件存在不等于抓到了目标文档；本例猜测的 `/peripherals/audio-es7210.html` 返回了标题为 `404 | 嘉立创` 的页面。核对页面标题，并从已确认页面提取真实章节链接。
- 开发板具有播放参考电路、板级源码设为 `AUDIO_INPUT_REFERENCE true`，不等于任意标称适配该板的二进制镜像正在运行同一代码，也不等于 AEC 始终开启或用户一定可以免按键自然插话。核对镜像版本、AEC 动态开关、唤醒与上行状态及设备实测。
- 不要从另一款组装底座的 BOM 推断它是在原装开发板上拆除了 ES7210/ES8311；先确认是同一块 PCB。单麦且未见物理回采，也不足以断言软件参考路径不存在。
- 单麦克风加单向 I2S 功放不等于必然无法做 AEC：播放数据也可能通过软件送作参考。不要仅凭未见物理回采线就断言必须换硬件、指定 `afe_config.ref_num`，或推定设备按键和唤醒词一定能在播放期间打断。
- 引用 BOM 文件名时保留实际完整文件名；本例为 `20250915-组装-BOM清单（不包含PCBA）.xlsx`，不能把缩写路径写成已核验来源。

## Verification

提交结论前，逐项检查：每个“当前支持”的主张都能定位到实际代码入口及处理分支；每个涉及设备行为、效果或性能的主张，都有独立验证依据或明确标注为未核验。
