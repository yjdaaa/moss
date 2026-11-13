# 第三方源码声明（Third-Party Notices）

MOSS 内置语音服务（见 `docs/integration/moss-native-voice-spec.md`）借用或移植了下列开源项目的源码或实现思路。按各项目许可证要求，在此保留原版权声明与许可文本。

- 借用代码的源文件，会在文件头部注明“来源项目 / 提交号 / 原文件路径”。
- 只参考行为、按其行为重写的模块，在模块文档注释中注明参考来源。
- 通过 Cargo 依赖引入的库（如 `sherpa-onnx`、`rmcp`、`opus2`）不属于本文范围，它们的许可证由各自的 crate 携带，可用 `cargo metadata` 或 `cargo about` 列出。

## 随包前端字体（UI-03）

Geist 与 Geist Mono 来自官方 [vercel/geist-font](https://github.com/vercel/geist-font)，固定提交 `10dc7658f13c38a474cde201bb09a4617267545b`。Copyright 2024 The Geist Project Authors；SIL Open Font License 1.1。未修改的 WOFF2、完整 `OFL.txt` 与来源/哈希说明位于 `frontend/public/fonts/`，随前端构建进入 Windows、Android 与 HTTP 宿主资源。字体不从外部 CDN 加载，中文仍回退到系统字体。

## 本地模型辅助程序与模型文件（NV6）

- **辅助程序 `moss-voice-local`**（`apps/moss-voice-local/`）以 **GPL-3.0-or-later** 单独发布，全文见 `apps/moss-voice-local/COPYING`。原因：它静态链接的 sherpa-onnx（Apache-2.0）预编译库包含 espeak-ng（GPL-3.0）。MOSS 本体（MIT）不链接它，只经标准输入输出通信；分发安装包时须一并提供该目录源码与 `COPYING`（技术书 §11.2，V5）。
- **模型文件不随 MOSS 分发**：由用户在语音中心按需从官方发布地址（或用户填写的镜像）下载，各自适用其上游许可：Silero VAD（MIT）、TEN-VAD（Apache-2.0 附加条款：不得以与 Agora 竞争的方式部署，详见 TEN-framework/ten-vad 的 LICENSE）、SenseVoice（模型目录内 `LICENSE`）、X-ASR 流式识别（Apache-2.0，Gilgamesh-J/X-ASR）、Kokoro v1.0 / v1.1（Apache-2.0，模型目录内 `LICENSE`）、Matcha 中英（Apache-2.0，ModelScope dengcunqin/matcha_tts_zh_en_20251010）及其声码器 `vocos-16khz-univ.onnx`（sherpa-onnx `vocoder-models` 发布）、Laya 多语种决策（Apache-2.0，ConvAIInnovations/laya-multilingual）、OneJev 1.6 0.8B / 4B / 9B GGUF（Apache-2.0，OmniJev/OneJev1.6-*-GGUF）。OneJev 按需下载的推理程序 `llama-server` 来自官方 [ggml-org/llama.cpp](https://github.com/ggml-org/llama.cpp) `b12167` 预编译包（MIT License，解压含上游 `LICENSE`），MOSS 只经本机回环 HTTP 调用、不链接。所有文件下载后均校验 SHA-256。

## 设备唤醒词模型与屏幕中文字库文件（n61 / n81 / n83）

- `crates/moss-voice-server/assets/esp-sr/` 下的 MultiNet6 中文模型 `mn6_cn`、`fst` 命令词表与 WakeNet9 `wn9_nihaoxiaozhi_tts` 原样取自乐鑫 [ESP-SR](https://github.com/espressif/esp-sr)（经 [78/xiaozhi-assets-generator](https://github.com/78/xiaozhi-assets-generator) 提交 `55517b40d724014faff00f941ca700cbf9d14b51` 转载，MIT，Copyright (c) 2025 Shenzhen Xinzhi Future Technology Co., Ltd.）。ESP-SR 使用 ESPRESSIF MIT License（仅限在乐鑫芯片产品上使用），全文见同目录 `LICENSE`。MOSS 只把它们打进下发给 ESP32 小智设备的资源包，桌面端不运行这些模型。资源包格式参照 xiaozhi-assets-generator 的 `SpiffsGenerator` / `WakenetModelPacker` 用 Rust 重写。
- `crates/moss-voice-server/assets/xiaozhi-fonts/` 下的 `font_noto_sans_common_{14_1,16_4,20_4,30_4}.bin` 与 `basic.json` 原样取自官方 [78/xiaozhi-fonts](https://github.com/78/xiaozhi-fonts) 提交 `d45dbc64052d57048f20ab1770074172ce9eb53b`（打包构建脚本为 Apache-2.0；字形源为 Google Noto Sans / Noto Sans CJK，SIL Open Font License 1.1，全文见同目录 `OFL.txt`）。服务端第 3 层全量动态补字分片包 `noto-v1-full.tar.gz`（含简/繁/日/韩/泰/阿 7 套分片 × 4 档字号，共 48,662 字，同属 `78/xiaozhi-fonts` 与 SIL Open Font License 1.1）不随安装包分发，由内置语音服务按需从官方 `https://files.xiaozhi.me/assets/fonts/noto-v1-full.tar.gz` 下载并校验 SHA-256 后用于 `glyph_push` v1 缺失字模动态下发。

## 借用或参考的项目

| 项目 | 仓库 | 锁定提交 | 许可证 | 用途 |
| --- | --- | --- | --- | --- |
| haimen | <https://github.com/shenjingnan/haimen> | `513d9e6` | MIT，Copyright (c) 2026 shenjingnan | 小智设备协议（二进制帧 v2/v3、消息类型）、播放节奏与连续性泵、切句与聚合、Claude Code / Codex 调用方式 |
| xiaozhi-esp32-server | <https://github.com/xinnan-tech/xiaozhi-esp32-server> | `7a260ee` | MIT，Copyright (c) 2025 xinnan-tech | 设备鉴权与 OTA 语义、VAD 参数、对话与工具调度、设备 MCP、插件、记忆、视觉、固件下发规则（按行为用 Rust 重写） |
| xiaozhi-client | <https://github.com/shenjingnan/xiaozhi-client> | `fe3c382` | MIT，Copyright (c) 2025 shenjingnan | 多接入点、工具命名与开关、第三方 MCP 聚合、心跳与重连（按思路用 Rust 重写） |
| univoice（Rust 版，`main-rs` 分支） | <https://github.com/shenjingnan/univoice> | `021113f` | MIT，Copyright (c) 2026 shenjingnan | 豆包流式识别与双向流式合成的二进制帧协议，已移植到 `crates/moss-voice-providers/src/doubao/{sauc.rs,bidirection.rs}`（NV7；按 crates.io 0.1.6 源码移植，与该提交对应） |
| OneJev | <https://github.com/OmniJev/OneJev> | `main`（1.6） | Apache-2.0，OmniJev | `qev-labels-v2` / `onejev-slot-v2` 结构化提示词格式与单 token 对数概率读出规则，已在 `crates/moss-voice/src/onejev.rs` 按协议用 Rust 重写 |

## 许可文本

以上四个项目都使用相同的 MIT 许可正文，只是版权行不同（见上表）：

```text
MIT License

Copyright (c) <年份> <版权人>

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

本文件中的许可正文已与四个仓库的 `LICENSE` 文件逐行核对（2026-10-07）。
