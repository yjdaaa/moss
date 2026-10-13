# 外部小智服务端版本与模型验证记录

更新：2026-10-11。对应 P0/P2a/P3。这里区分源码核验、假服务验证与真实联调；
不填写真实地址、账号、密钥或录音。实际配置仅在用户授权的本机配置中维护。

## 1. 固定上游源码版本（P0-05）

| 项目 | 固定提交 | 许可证据 | 当前证据范围 |
| --- | --- | --- | --- |
| xiaozhi-esp32-server | `87c6df77b0220ddbd8fc804f984f83f69f3225e5` | [MIT](https://github.com/xinnan-tech/xiaozhi-esp32-server/blob/87c6df77b0220ddbd8fc804f984f83f69f3225e5/LICENSE) | 原有源码阅读记录；本次核对完整提交，不代表已部署 |
| mcp-endpoint-server | `0f9302f327100fb976c7bdea6e4ebbb8ebdb5787` | [MIT](https://github.com/xinnan-tech/mcp-endpoint-server/blob/0f9302f327100fb976c7bdea6e4ebbb8ebdb5787/LICENSE)，Copyright (c) 2025 xinnan-tech | 本次公开仓库提交与许可证核对；运行握手留 P2a |
| xiaozhi-client | 未纳入本次实施 | P5 未决定 | 可选项不作为 P0 必需版本 |

版本锁定仅供未来明确部署时复现；不会自动检出、安装或启动上游服务。
部署时若采用不同提交，应重新记录接口/许可和运行验证，不把固定提交冒充设备当前版本。

## 2. 已有软件保护与本轮可重跑的假服务检查

| 范围 | 现有实现与测试 | 验证边界 |
| --- | --- | --- |
| LLM/ASR/TTS 请求 | `moss-voice/src/models.rs`：OpenAI 兼容路径、流式工具请求、ASR multipart、TTS 音频、错误/跳转/脱敏测试 | 本机假 HTTP，不能证明供应商能力或中文质量 |
| 测试后保存 | `moss-voice/src/center.rs`：成功测试 fingerprint 必须匹配；失败保留旧配置 | 临时目录与假服务，不接用户服务端 |
| 分区与回滚 | `moss-voice/src/config_yaml.rs`：仅改选中分区，备份/原子替换，manager-api 模式拒绝改写 | 不读取用户真实 YAML 或密钥 |
| 接入点状态 | `center.rs`：health 合同；不可达标 unknown | 合成计数不能证明实际握手 |

具体运行结果只在实施 TODO §9 追加，不从测试存在推断本轮通过。

## 3. 真实模型验证表（P0-01～04 / P3，均未验收）

| 项目 | 协议要求 | 需记录的非敏感结果 | 状态 |
| --- | --- | --- | --- |
| LLM | `/chat/completions`、中文、stream、tools/tool_calls | 模型公开名称、样本编号、首包/整句毫秒、参数准确率、错误码 | external-live-unverified |
| ASR | `/audio/transcriptions`、multipart、采样率/格式 | 中文样本编号、识别结果评定、延迟、错误码 | external-live-unverified |
| TTS | `/audio/speech`、模型/音色、输出格式 | 中文样本编号、音频格式、首包/总耗时、人工听音评定 | external-live-unverified |
| 整轮 | ASR + LLM + TTS | 同一条件下至少 5 轮数据，用户接受的延迟预算 | external-live-unverified |
| 配置/恢复 | 分区备份、原子写、失败回滚 | 成功/失败、未选中分区是否保持、回退结果 | external-live-unverified |

没有受授权的真实服务/账号时保持未验；不发送测试录音或调用计费 API。
P0-06 设备固件、P0-08 部署位置和 P0-09 可选范围也继续待用户实际信息/决定。

## 4. 外部链路执行顺序

先验证 P0 → 获授权部署 P2a → 保留 P1 执行层门禁 → 按分区完成 P3。
P2b 是可选增强，只有实际接入点不兼容且选择替代链路时才成为前置。
自建提示词见 [P2a-05 文本](xiaozhi-prompts/history/2026-10-11-self-hosted.md)；
仅提供可审阅文档，本次不写入外部服务端。
