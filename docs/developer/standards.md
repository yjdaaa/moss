# 开发规范

实现层约定。安全边界、验证、结果状态和 CHANGELOG 规则见 [`AGENTS.md`](../../AGENTS.md)。

## 项目结构

| 目录 | 职责 |
| --- | --- |
| `crates/laser-contracts` | 跨 crate 共享的强类型模型与 JSON Schema，无内部依赖 |
| `crates/laser-core` | 纯算法（字体、光栅、矢量、G-code、材料推荐、参数范围），不做 I/O |
| `crates/laser-device` | 串口 / Telnet 传输、握手、预检、流控、实时字节、`FakeGrbl` |
| `crates/laser-runtime` | `.env` 配置、运行目录、SQLite schema、`RuntimeStore`、旧数据迁移 |
| `crates/laser-tools` | `ToolCatalog` 与不依赖宿主的工具实现 |
| `crates/laser-mcp` | JSON-RPC 服务、工具分发、小智 WebSocket 桥 |
| `crates/laser-api` | HTTP 路由、workflow 状态机、发送门禁、材料实验室、配对鉴权、嵌入前端 |
| `crates/laser-host` | 启动组装、退出协调、语音工具与审批接线 |
| `crates/moss-xiaozhi-proto` | 小智设备协议：消息、音频帧、设备 token、OTA |
| `crates/moss-voice-server` | 内置语音服务：OTA / WebSocket、设备允许、会话、插件（仅桌面） |
| `crates/moss-voice-providers` | ASR / LLM / TTS 提供方（OpenAI 兼容、本地模型、豆包流式） |
| `crates/moss-voice` | 语音中心设置、外部 xiaozhi-server 进程管理与模型配置（仅桌面） |
| `crates/moss-voice-local-proto` | 与本地模型 helper 的帧协议 |
| `crates/moss-mcp-client` | 第三方 MCP 客户端（stdio / Streamable HTTP），默认拒绝 Ask |
| `apps/moss-host` | 命令行宿主 `serve` / `migrate` |
| `apps/desktop` | Tauri 2 桌面与 Android 外壳 |
| `apps/moss-voice-local` | 本地模型 helper（GPL-3.0，独立 Cargo 工作区，不在根 workspace） |
| `frontend/` | React + Vite；`tests/*.test.mjs` 单测，`tests/e2e/` 端到端 |
| `tests/golden/` | 黄金样本；`mcp_tools.json` 被 `laser-mcp` 编译期嵌入 |
| `tests/fixtures/` | 字体（编译期嵌入）与示例图片 |
| `scripts/` | 启动器、桌面热重载与快速同步、E2E 调度、安装包构建、NV13/16/17/18 验证与许可收集脚本 |

分层与依赖方向见 [架构总览](architecture.md)。只有多个 crate 确实复用时才下沉到 `laser-core` 或 `laser-contracts`。

## 代码约定

- 新增或删除激光 MCP 工具：同步 `ToolCatalog`（`PRIORITY_TOOLS` / `SECONDARY_TOOLS`）、`dispatch_tool_call_with`、`laser-contracts` 参数结构与 `risk.rs::tool_risk` 分级、`tests/golden/mcp_tools.json` 及 `moss-voice-server/src/jev.rs::groups_of_builtin_tool` 粗筛归组。
- 需要持久化或真实设备的工具经 `HostBackend` 转发到 `ApiService` 现有路由，与网页共用门禁；不在 `laser-tools` 直连数据库或设备。
- 内置语音新工具由 `ToolExecutor` 组合，名称不含 `__`（`device__` 保留给设备端硬件 MCP，其他 `<server>__<tool>` 分流到第三方 MCP）；密钥只存 `RuntimeDb::app_secrets`。
- Jev 工具与上下文筛选（`moss-voice-server/src/jev.rs`）：工具超过 20 个时严禁按单工具平铺 `noul`（避免约 3% 独立误报率累积漏入无关工具），内置与扩展工具统一收拢为既有 6 个业务域粗筛组（删除工具时按当轮实际启用工具动态提问，新增工具通过静态表 + 名称/描述自动归组兜底）；第三方外部 MCP 少量（`<= 6`）合并为单开关，较多（`> 6` 且跨多服务）时按 `<server>` 生成 Server 级开关；`state` 单条候选恒 `<= 600` 字符、仅带最近 2 轮极简文本上下文。
- `laser-tools`、`laser-device`、`laser-core` 不依赖 `serde_json`、不用 `Value` 动态访问；`crates/laser-contracts/tests/type_boundary.rs` 会拒绝。
- 环境变量集中在 `laser_runtime::LaserSettings` 与 `laser_tools::resolve_connection_spec` 读取。
- 数据库迁移加在 `RuntimeDb::migrate`，事务内执行、可重跑，使用实际下一可用版本号。
- 新增 Tauri 命令：在 `apps/desktop/src-tauri/build.rs` 的 `AppManifest::commands` 登记，并加入对应 capability（主窗口 `default.json`，私有窗口 `voice-*.json`）；私有命令同步 `frontend/tests/voicePrivacyAcl.test.mjs`。漏登记会在运行时被 ACL 拒绝，曾导致启动失败和黑屏。
- 会创建窗口的 Tauri 命令必须写成 `async fn`：Windows 上在同步命令里建窗口会让主线程死锁（新窗口白屏、程序无响应），`moss-desktop` 的 `window_creating_commands_are_async` 会检查。
- 前端确认一律 `await showConfirm(...)`（`appDialog.js`），不要直接用 `window.confirm` / `window.alert`：桌面端 Tauri 对话框插件把它们换成了返回 Promise 的异步函数，直接当布尔值会在用户回答前就判成“确认”。新页面入口要挂 `<AppDialogHost />`。
- PowerShell 脚本保持纯 ASCII：Windows PowerShell 5.1 按系统代码页读取无 BOM 文件，中文会导致解析失败。
- 桌面热重载（`scripts/dev-desktop.ps1`）：仅 debug 构建读取 `MOSS_DEV_UI_ORIGIN`（固定白名单 `http://127.0.0.1:5177` / `5173`）让主窗口连接 Vite 热更新；release 构建恒忽略该变量，`voice-private` 窗口始终加载本地打包页 `tauri.localhost`。
- 平台差异显式分支，不支持时返回明确原因。
- 工具名、HTTP 路由、错误码、响应字段和用户可见文案都是契约：改动前更新 [Web API 契约](api-contracts.md) 与测试断言。
- Web 服务默认只绑回环；非回环依赖配对鉴权。

## 文档

- 目录：用户操作 `docs/user/`，开发与契约 `docs/developer/`，硬件固件 `docs/laser/`，集成方案与进度 `docs/integration/`。
- 小智提示词只改 `docs/integration/xiaozhi-prompts/latest.md`，并新增日期快照。
- 功能可见变化同步 [功能字典](feature-glossary.md)。
- 不写会随构建变化的数字（测试数、产物大小）；这类信息放 CHANGELOG 验证段或 [构建回执](build-receipts.md)。

## PR 检查清单

- [ ] 改动符合目录职责，没有跨层直连设备或数据库。
- [ ] 工具增删已同步 `ToolCatalog`、分发分支、`tool_risk`、`mcp_tools.json` 与 `jev.rs` 粗筛归组（不按单工具平铺 `noul`）。
- [ ] 外部效果使用 mock / `FakeGrbl`，未连接真实设备。
- [ ] 未提交 `.env`、运行目录、构建产物或个人配置。
- [ ] 已按 `AGENTS.md` 运行验证，跳过项写明原因；CHANGELOG 与相关 `docs/` 已更新。
