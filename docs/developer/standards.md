# 开发规范

实现层约定。安全边界、验证、结果状态和 CHANGELOG 规则见 [`AGENTS.md`](../../AGENTS.md)。

## 项目结构

| 目录 | 职责 |
| --- | --- |
| `crates/laser-contracts` | 跨 crate 共享的强类型模型与 JSON Schema，无内部依赖 |
| `crates/laser-core` | 纯算法（字体、光栅、矢量、G-code、材料推荐、参数范围），不做 I/O |
| `crates/laser-device` | 串口 / Telnet 传输、握手、预检、流控、实时字节、`FakeGrbl` |
| `crates/laser-runtime` | 激光与语音共用的运行时层：`.env` 配置、运行目录、SQLite schema 与各类存储（Agent / 上下文 / 知识库 / 固件）、`RuntimeStore`、旧数据迁移 |
| `crates/laser-tools` | `ToolCatalog` 与不依赖宿主的工具实现 |
| `crates/laser-mcp` | JSON-RPC 服务、工具分发、小智 WebSocket 桥 |
| `crates/laser-api` | HTTP 路由、workflow 状态机、发送门禁、材料实验室、配对鉴权、嵌入前端 |
| `crates/laser-host` | 启动组装、退出协调、语音工具分发与第三方 MCP 接线 |
| `crates/moss-agent` | 本机 Agent（NV13）CLI 识别、受限进程运行器与桌面审批桥（仅桌面） |
| `crates/moss-home` | Home Assistant 家居控制（NV15）策略、审批与 HTTPS 传输（仅桌面） |
| `crates/moss-process` | 第 0 层跨平台子进程生命周期守卫：Windows Job Object（`KILL_ON_JOB_CLOSE`）进程树托管句柄 |
| `crates/moss-usb-provision` | 小智 USB 一键配网：读写 NVS、回读校验与失败恢复、预签设备令牌（仅桌面） |
| `crates/moss-xiaozhi-proto` | 小智设备协议：消息、音频帧、设备 token、OTA |
| `crates/moss-xiaozhi-display` | 小智彩屏渲染、护目镜小机器人动图、中文字库与全屏画风/唤醒词 `assets.bin` 纯构造（零网络与异步依赖） |
| `crates/moss-voice-server` | 内置语音服务：OTA / WebSocket、设备允许、会话、插件（仅桌面） |
| `crates/moss-voice-providers` | ASR / LLM / TTS 提供方（OpenAI 兼容、本地模型 `moss-voice-local` / `speech.cpp` / `audio.cpp`、豆包流式、千问 / 百炼原生 SSE 流式合成与声音克隆） |
| `crates/moss-voice` | 语音中心设置与链路切换（官方 / 内置）、内置服务的模型配置与本地模型管理（仅桌面） |
| `crates/moss-voice-host` | 内置语音与长期记忆宿主总装配（零 Tauri/UI 依赖，`BuiltinVoice` / `ContextHost` 编排，仅桌面） |
| `crates/moss-voice-local-proto` | 与本地模型 helper 的帧协议 |
| `crates/moss-mcp-client` | 第三方 MCP 客户端（stdio / Streamable HTTP），默认拒绝 Ask |
| `apps/moss-host` | 命令行宿主 `serve` / `migrate` |
| `apps/desktop` | Tauri 2 桌面与 Android 外壳 |
| `apps/moss-voice-local` | 本地模型 helper（GPL-3.0，独立 Cargo 工作区，不在根 workspace） |
| `frontend/` | React + Vite；`tests/*.test.mjs` 单测，`tests/e2e/` 端到端 |
| `tests/golden/` | 黄金样本；`mcp_tools.json` 被 `laser-mcp` 编译期嵌入 |
| `tests/fixtures/` | 字体（编译期嵌入）与示例图片 |
| `scripts/` | 根目录放日常入口：统一验证 `verify.mjs`、CHANGELOG 归档 `archive-changelog.mjs`、crate 分层 `check-crate-layers.mjs`、仓库规范 `check-repo-hygiene.mjs`、启动器、桌面热重载与快速同步、缓存清理、E2E 调度、安装包构建、发版；另有图标与架构图生成、合成音频夹具生成等开发工具 |
| `scripts/voice-helper/` | 带语音 helper 的专用安装包构建、包内容核查、原生依赖许可审计与源码收集（NV18） |
| `scripts/agent-sandbox/` | 本机 Agent 的 Docker 隔离测试镜像与验收脚本（NV13） |
| `scripts/speaker/` | 声纹标定与 helper 实测（NV16） |
| `scripts/voice-bench/` | 整条语音链路实测的说话片段生成与结果汇总（配合 `moss-voice-server` 的 `real_local_chain_matrix`）；`endpoint-review.mjs` 从完整过程记录统计判停抢话与白等（n112） |
| `scripts/laser-bench/` | 激光雕刻机 AI 自主测试（L0 隔离回环模拟机全自动与不变量检查、L1 真机只读白名单核对与脱敏归档，r74） |
| `scripts/tests/` | 脚本单测，CI 以 `node --test scripts/tests/*.test.mjs` 运行 |
| `.githooks/` | 受管本地 Git 钩子（`pre-commit`、`commit-msg`），通过 `node scripts/verify.mjs --install-hooks` 启用 |

分层与依赖方向见 [架构总览](architecture.md) §2，由 `scripts/check-crate-layers.mjs` 把关；新增 crate 先在该脚本的 `LAYERS` 登记层级。脚本与文档按用途命名，任务代号（NVxx）只写在正文里。只有多个 crate 确实复用时才下沉到 `laser-core` 或 `laser-contracts`。

## 代码约定

- 新增或删除激光 MCP 工具：同步 `ToolCatalog`（`LASER_TOOLS` / `DESKTOP_TOOLS`）、`dispatch_tool_call_with`、`laser-contracts` 参数结构与 `risk.rs::tool_risk` 分级、`tests/golden/mcp_tools.json` 及 `moss-voice-server/src/jev.rs::groups_of_builtin_tool` 粗筛归组。
- 需要持久化或真实设备的工具经 `HostBackend` 转发到 `ApiService` 现有路由，与网页共用门禁；不在 `laser-tools` 直连数据库或设备。
- 内置语音新工具由 `ToolExecutor` 组合，名称不含 `__`（`device__` 保留给设备端硬件 MCP，其他 `<server>__<tool>` 分流到第三方 MCP）；密钥只存 `RuntimeDb::app_secrets`。
- Jev 工具与上下文筛选（`moss-voice-server/src/jev.rs`）：工具超过 20 个时严禁按单工具平铺 `noul`（避免约 3% 独立误报率累积漏入无关工具），内置与扩展工具统一收拢为既有 6 个业务域粗筛组（删除工具时按当轮实际启用工具动态提问，新增工具通过静态表 + 名称/描述自动归组兜底）；第三方外部 MCP 少量（`<= 6`）合并为单开关，较多（`> 6` 且跨多服务）时按 `<server>` 生成 Server 级开关；`state` 单条候选恒 `<= 600` 字符、仅带最近 2 轮极简文本上下文。
- `laser-tools`、`laser-device`、`laser-core` 不依赖 `serde_json`、不用 `Value` 动态访问；`crates/laser-contracts/tests/type_boundary.rs` 会拒绝。
- 环境变量集中在 `laser_runtime::LaserSettings` 与 `laser_tools::resolve_connection_spec` 读取。
- 数据库迁移加在 `RuntimeDb::migrate`，事务内执行、可重跑，使用实际下一可用版本号。
- 新增 Tauri 命令：在 `apps/desktop/src-tauri/build.rs` 的 `AppManifest::commands` 登记，并加入对应 capability（通用命令在 `default.json`，主窗口专属特权命令在 `voice-*.json` 并固定精确后端来源）；敏感语音命令同步 `frontend/tests/voicePrivacyAcl.test.mjs`。漏登记会在运行时被 ACL 拒绝，曾导致启动失败和黑屏。
- 会创建窗口的 Tauri 命令必须写成 `async fn`：Windows 上在同步命令里建窗口会让主线程死锁（新窗口白屏、程序无响应），`moss-desktop` 的 `window_creating_commands_are_async` 会检查。
- 前端确认一律 `await showConfirm(...)`（`appDialog.js`），新页面入口挂 `<AppDialogHost />`；直接调用 `confirm`/`alert`、`window.confirm`/`alert` 会被 `frontend/eslint.config.js` 拦截（桌面端原生确认框返回 Promise，直接判布尔值会跳过确认）。
- `.ps1` 纯 ASCII（兼容 PowerShell 5.1）、禁整文件 `#![allow(clippy::...)]`、敏感路径与提示词字数上限由 `scripts/check-repo-hygiene.mjs` 检查；`[lints] workspace = true` 与 `test-memory-secret-store` 仅限 `[dev-dependencies]` 由 `scripts/check-crate-layers.mjs` 检查。
- 桌面热重载（`scripts/dev-desktop.ps1`）：仅 debug 构建读取 `MOSS_DEV_UI_ORIGIN`（固定白名单 `http://127.0.0.1:5177` / `5173`）让主窗口连接 Vite 热更新；release 构建恒忽略该变量。
- 平台差异显式分支，不支持时返回明确原因。
- 工具名、HTTP 路由、错误码、响应字段和用户可见文案都是契约：改动前更新 [Web API 契约](api-contracts.md) 与测试断言。
- Web 服务默认只绑回环；非回环依赖配对鉴权。
- 格式与静态检查（2026-11-07 加门禁）：Rust 用 `rustfmt` 默认规则（无 `rustfmt.toml`），`cargo clippy --workspace --all-targets -- -D warnings` 零警告（结构提示放行项见根 `Cargo.toml`，参数上限 10 见 `clippy.toml`，局部例外用带 `reason` 的 `#[expect(...)]`）。前端 `npm run lint` 零 error：Hooks 只能在组件顶层调用，“关闭即重置”的弹窗写外层判断 `visible`、内层组件放 Hooks；`react-hooks/exhaustive-deps` 暂为 warning，逐步清理。
- 纯格式化的大提交记入 `.git-blame-ignore-revs`，本机执行一次 `git config blame.ignoreRevsFile .git-blame-ignore-revs`。
- 测试里按源码文本断言时，正则要容忍 rustfmt 可能加的空白（如 `,\s*`），不要依赖紧凑写法。
- FakeGrbl 端到端调度器只探测本轮启动的宿主报告的回环端口。宿主提前退出、启动超时或进程收到中断时，按失败收尾，等待所启动的子进程退出后再清理临时目录。

## 文档

- 目录：用户操作 `docs/user/`，开发与契约 `docs/developer/`，硬件固件 `docs/laser/`，集成方案与进度 `docs/integration/`。
- 小智提示词只改 `docs/integration/xiaozhi-prompts/latest.md`，并新增日期快照。
- 功能可见变化同步 [功能字典](feature-glossary.md)。
- 不写会随构建变化的数字（测试数、产物大小）；这类信息放 CHANGELOG 验证段或 [构建回执](build-receipts.md)。

## PR 检查清单

- [ ] 改动符合目录职责，没有跨层直连设备或数据库。
- [ ] 工具增删已同步 `ToolCatalog`、分发分支、`tool_risk`、`mcp_tools.json` 与 `jev.rs` 粗筛归组（不按单工具平铺 `noul`）。
- [ ] 外部效果使用 mock / `FakeGrbl`，未连接真实设备。
- [ ] 会经过 `laser-runtime` 家居令牌封存的测试只在 `[dev-dependencies]` 开启 `test-memory-secret-store`（macOS 上改用进程内主密钥，不碰登录钥匙串）。
- [ ] 收尾级 `node scripts/verify.mjs --finish`（分层、规范、格式化、clippy、eslint、受影响测试，按改动自动追加浏览器测试 / E2E 与桌面快更）已通过，跳过项写明原因；CHANGELOG 与相关 `docs/` 已更新。
