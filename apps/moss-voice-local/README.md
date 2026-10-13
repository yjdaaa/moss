# moss-voice-local

MOSS 内置语音服务的本地模型辅助程序（内置语音技术书 §11.2，NV6；可选模型见 §13.10）：判停（Silero / TEN-VAD）、识别（SenseVoice 整段 / X-ASR 流式）、合成（Kokoro / Matcha）。

## 许可证

本程序以 **GPL-3.0-or-later** 发布（全文见同目录 `COPYING`），与 MOSS 本体（MIT）是两个独立程序：

- 它静态链接 [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx)（Apache-2.0）的预编译库，其中包含 espeak-ng（GPL-3.0），因此整个辅助程序受 GPL-3.0 约束；
- MOSS 只通过标准输入输出与它交换数据（帧协议见 `crates/moss-voice-local-proto`，MIT），不链接它的代码；
- 分发 MOSS 安装包时须同时提供本目录的源码与 `COPYING`。

以上是对许可证条款的一般理解，不构成法律意见。

## 构建

独立 Cargo 工作区，不参与 MOSS 根目录的 `cargo test --workspace`。在仓库根目录执行（产物与 `moss-desktop.exe` 同在 `target/release/`）：

```bash
cargo build --release --manifest-path apps/moss-voice-local/Cargo.toml --target-dir target
```

首次构建会从 GitHub 下载 sherpa-onnx 预编译静态库；离线构建可设置 `SHERPA_ONNX_LIB_DIR`。

## 协议

由 MOSS 启动，经标准输入输出按 `moss-voice-local-proto` 的帧格式（长度前缀的 JSON 头，协议 4）通信；音频负载为16位PCM，声纹向量为192维 f32le。标准输入关闭即退出。判停、识别、合成、声纹在独立线程；宿主声纹另启独立进程，避免声纹超时终止其他语音。`moss-voice-local --version` 打印版本与协议号。协议4必须与本轮 MOSS 一起升级，旧辅助程序会被握手拒绝。

**NV8a 升级要求**：本次协议从 2 升为 3，必须按上方命令重建辅助程序，并与新的 MOSS 一起更新；旧辅助程序会被版本握手拒绝，不能沿用。协议 3 增加识别解码选项 / 热词、中间识别文字和 `synthesize_stream` 分段音频响应。X-ASR 可使用 `modified_beam_search`；热词仅采用模型词表能表达的短语，SenseVoice 不支持这一热词通路。分段合成在每个分段生成后返回音频，不宣称是声学模型内部逐采样流式推理。

取消某轮后，宿主丢弃该轮响应；辅助程序正在执行的单次同步推理可能仍会完成，不能把它解释为已抢占底层推理线程。

模型由 MOSS 语音中心下载到运行目录 `models/` 并校验 SHA-256，本程序只按 MOSS 给出的路径加载。
