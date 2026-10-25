# 架构总览

本文描述当前仓库结构，依据根级 workspace、各 `Cargo.toml`、源码与测试整理。

## 1. 进程与入口

| 进程 | 位置 | 说明 |
| --- | --- | --- |
| `moss-desktop` | `apps/desktop/src-tauri` | Tauri 2 外壳。`run()` 先安装 panic 钩子与控制台关闭钩子，再在进程内初始化 `laser_host::HostApplication`，启动 HTTP 服务与 MCP 桥，最后创建窗口。桌面平台带系统托盘；关闭窗口被拦截并通知前端弹出"托盘 / 退出"选择。Android/iOS 通过 `mobile_entry_point` 复用同一份代码，前端资源来自 `frontend/dist` |
| `moss-host` | `apps/moss-host` | 命令行宿主。`serve` 启动 HTTP 服务与 MCP 桥并阻塞；`migrate --from <旧根>` 迁移旧 Python 版运行数据；无参数时只跑三项启动自检 |
| 前端 | `frontend/` | React 18 + Vite 6 + Excalidraw 0.18。路由 `/`、`/draw`、`/material-lab`（桌面端另有 `/voice`）由 `main.jsx` 按 `pathname` 前缀切换。生产资源由 `laser-api` 通过 `rust-embed` 嵌入并从 `/`、`/assets/*` 提供；Tauri 内嵌页启动时用 `get_backend_origin` 获取实际回环端口（失败则显示错误），桌面同源页使用当前地址；独立 Vite 开发页仍回退到 `http://127.0.0.1:9666` |

## 2. crate 分层

依赖只允许向下。`crates/laser-contracts/tests/type_boundary.rs` 随测试运行，禁止 `laser-tools`、`laser-device`、`laser-core` 依赖 `serde_json` 或使用 `Value` 动态访问器，所有跨 crate 数据必须经 `laser-contracts` 的强类型结构。

```text
apps/moss-host / Tauri ──► laser-host ──► laser-api ──► laser-runtime ──► laser-contracts
                           │                 ├──► laser-core ──► laser-contracts
                           │                 └──► laser-device（串口/Telnet、SD 上传）
                           └──► laser-mcp ──► laser-tools ──► laser-core
                                    │               └──► laser-device（独立安全动作/探测）
                                    └──► HostBackend（laser-host 实现）──► laser-api
```

| crate | 职责 | 关键符号 |
| --- | --- | --- |
| `laser-contracts` | 纯数据模型与 JSON Schema：材料库、workflow / job / text_task / calibration 记录、工具参数与 `ToolCallResult`、MCP 遥测单例、内置材料库；调用方上下文、审批与审计类型、工具风险分级 | `MaterialLibrary`、`WorkflowRecord`、`ToolCallResult`、`builtin_material_library`、`CallerContext`、`ApprovalRecord`、`AuditEntry`、`tool_risk` |
| `laser-core` | 纯算法：字体轮廓（`ttf-parser`）、位图抖动与光栅规划、矢量描边与填充、G-code 生成与时间估算、材料推荐与标定矩阵、参数安全范围 | `gcode::write_raster` / `write_outline` / `generate_framing_gcode`、`raster::dither`、`material::calibration_matrix`、`safety::validate_core_job` / `evaluate_material` |
| `laser-device` | 硬件通信：串口与 TCP 传输、有界握手、FluidNC `$CD` 配置解析、Idle 与引脚预检、字符计数流控与 `Bf:` 自适应窗口、S 值按 `$30` 换算、实时字节、mDNS 发现、SD 卡 HTTP 上传、错误码翻译、`FakeGrbl` 测试桩 | `DeviceTransport`、`ConnectionSpec`、`DeviceConnection`、`JobWorker`、`authorize_send`、`RealtimeCommand`、`discover_controllers` |
| `laser-runtime` | 配置与持久化：`.env` 读取、运行目录解析与实例锁、SQLite schema 与迁移、`RuntimeStore` 抽象、旧数据迁移器 | `LaserSettings`、`RuntimeRoot`、`RuntimeDb`、`RuntimeStore`（`SqliteStore` / `MemoryStore`）、`run_migration` |
| `laser-tools` | MCP 工具目录与不依赖宿主的工具实现：连接检查、安全动作、参数推荐、文字图片、桌面应用与快捷键、开发者命令 | `ToolCatalog`、`execute_check_laser_connection`、`execute_laser_safe_action`、`resolve_connection_spec`、`developer_mode_enabled` |
| `laser-mcp` | JSON-RPC 2.0 MCP 服务：`initialize` / `ping` / `tools/list` / `tools/call`，工具分发，连接小智控制台的 WebSocket 客户端（`tungstenite` + `rustls`，同步实现） | `tools_list_json`、`dispatch_tool_call_with`、`HostBackend`、`run_bridge`、`endpoint_from_env` |
| `laser-api` | HTTP 服务与业务编排：所有 `/api/*`、`/v1/*` 路由，workflow 状态机，发送门禁与 `JobWorker` 调度，材料实验室，局域网配对，桌面会话、严格模式与桌面审批（`approval.rs`），前端静态资源 | `ApiService`、`AuthManager`、`abort_all_active_jobs`、`ApprovalService`、`require_r3_authority` |
| `laser-host` | 组装层：初始化运行目录、配置、数据库、`ApiService`，启动 HTTP 与 MCP 桥，全局退出协调器 | `HostApplication`、`InProcessBackend`、`ShutdownCoordinator`、`install_panic_hook`、`install_console_ctrl_handler` |

## 3. HTTP 服务

- `ApiService::serve` 在 `std::net::TcpListener` 上手写解析 HTTP/1.1，每个连接一个线程，读超时 10 秒。没有 tokio、axum 或任何异步运行时。
- 首选端口来自 `LASER_WEB_PORT`，否则运行目录 `settings.json` 的 `web_port`，否则 `9666`。绑定失败时向后尝试 50 个端口，再失败则由系统分配；`/api/system/info` 返回 `preferred_port`、`actual_port` 与 `is_fallback`。
- 鉴权：回环地址免鉴权。非回环时，`GET /api/health`、`/v1/capabilities`、`/api/system/info` 与页面/静态资源公开，其余路由要求 `Authorization: Bearer`。配对码由 `POST /v1/auth/challenge`（仅回环）签发，`POST /v1/auth/pair` 换取 token；token 与配对状态只在 `AuthManager` 内存中，进程重启即失效。
- 调用方上下文（`CallerContext`，技术书 §6.4）只由入口构造：带有效 `X-Moss-Desktop-Session` 的回环请求为 `Desktop`，其他回环请求为 `Loopback`，局域网配对为 `Lan`，进程内小智桥接为 `McpBridge`，内置语音服务为 `Voice { device_id, session_id }`（`laser_host::VoiceBackend`，NV4），审批执行为 `Approval`。请求体里的身份字段在入口删除。
- `/v1/events` 是一次性 SSE：返回单条 `event: ready` 后关闭连接。前端不使用它，进度通过轮询 `POST /api/workflow {action:"status"}` 获得。
- 路由清单与字段见 [Web API 契约](api-contracts.md)。

## 4. MCP 桥

- 先读运行目录 `settings.json` 的 `mcp_endpoint`，未配置时才回退 `MCP_ENDPOINT` 环境变量；有效地址须为 `ws://` 或 `wss://` 且不含 `<` 占位符，否则静默跳过。
- 桥在独立线程中运行 `laser_mcp::run_bridge`，断线按 1 秒起、60 秒封顶指数退避重连，日志中的 token 脱敏。
- 协议版本 `2024-11-05`，服务名 `YOKO_MCP_SERVER`。`tools/list` 使用编译期嵌入的黄金清单，并与 `ToolCatalog` 校验一致；清单共 20 个工具，未设置 `MOSS_ENABLE_DEVELOPER_COMMANDS=true` 时不含 `command_execution_tool`，共 19 个。
- 需要持久化或真实作业的工具（workflow、材料库、标定矩阵、文字任务、正式发送）通过 `HostBackend` 转发到进程内 `ApiService`，与网页共用同一套门禁、SQLite 与 `JobWorker`。`laser_grbl_tool` / `laser_network_grbl_tool` 的 `send_file`、`send_command` 直接拒绝并引导走 workflow 或安全动作。
- 桥的连接状态、客户端身份与最近请求写入 `laser_contracts::global_mcp_telemetry`，由 `/api/system/info` 暴露给前端。

## 4a. 严格模式与桌面审批（P1）

- 开关 `MOSS_FUSION_STRICT`（默认 `false`）。关闭时下述检查一律不生效，行为与之前一致；`MOSS_GATEWAY_ENABLED=true` 而严格模式关闭时宿主拒绝启动。
- 唯一检查点 `ApiService::require_r3_authority`，接在全部 R3 执行入口：`launch_send_job_for`（workflow / 文字任务 / 标定矩阵发送与断点续雕）、`/api/safe-action` 的 R3 动作、`/api/device/framing`。桌面会话放行；小智桥接 / 局域网 / 网关创建审批并返回 202 `approval_required`；本机回环无凭据返回 403 `strict_mode_blocked`；审批身份按当前要素与保存的连接重算摘要并单次消费，执行使用审批保存的连接参数。
- `ApprovalService`（`crates/laser-api/src/approval.rs`）管理审批生命周期，数据在 SQLite `approvals` 表（schema v4/v5）；创建、批准、执行与审计在同一事务中，审计写不进去就拒绝。经审批的发送在启动前记 `phase=running`，作业结束由发送线程回写；进程重启时待批准 / 已批准的过期，仍在发送的记为结果未知，均不重放。
- 桌面审批接口 `/v1/approvals*` 只接受桌面会话；会话值由 Tauri 壳启动时生成（`get_desktop_session`），独立 `moss-host serve` 没有桌面会话，严格模式下只能拒绝或等待过期。
- 小智接入点按 `laser_contracts::tool_risk` 分级：严格模式下 R4 与未分级工具直接拒绝，R3 交给执行层生成审批。前端全局审批卡见 [前端行为契约](frontend-behavior.md) §8。

## 4b. 语音中心（P4，仅桌面端）

- crate `crates/moss-voice` 不依赖 Tauri，只由 `apps/desktop/src-tauri` 在非 Android / iOS 目标引入（`src/voice.rs`，Tauri 命令见 [接口契约](api-contracts.md) §5）；`laser-api`、`moss-host`、Android 不包含进程管理代码。
- `VoiceCenter` 编排页面操作；MOSS 后端的能力经 `HostHooks` 交入：严格模式开关（`ApiService::set_strict_mode`，`.env` 开启的不能被关掉）、小智桥重连（`HostApplication::restart_mcp_bridge`：先停旧桥、等待其退出，再按 `settings.json` 接新桥）、桥连接状态、设备会话数（写 `laser_contracts::global_voice_telemetry`，`/api/system/info` 的 `voice.device_sessions` 读取）。
- 链路切换：官方 / 自建互斥；切到自建先开严格模式，再一次写入 `settings.json` 的 `voice` 与 `mcp_endpoint`（备份官方地址），再重连桥；任一步失败恢复设置、严格模式与原来的桥。宿主启动时 `voice.mode = self_hosted` 也会开启严格模式。
- 服务端进程：`Supervisor` 单实例、端口探测识别外部实例、停止先正常退出 5 秒后强制、Windows Job Object（随 MOSS 关闭结束进程树；用户退出时选择保留则解除）、崩溃不自动重启；输出脱敏后写 `logs/xiaozhi-server.log`。
- 退出顺序：`ShutdownState::request()`（急停与任务清零）→ 处理服务端（停止或保留）→ 退出。托盘“退出程序”在服务端运行时用对话框询问。
- 模型向导：测试连接用 `reqwest` 阻塞客户端（15 秒超时、不跟随重定向，错误信息不含地址与密钥）；保存要求与最近一次成功测试的内容一致，按行读-改-写 `data/.config.yaml`（保留注释，`.bak` 备份，`serde_yaml` 重新解析核对），全模块部署（`manager-api.url`）拒绝写入。密钥不进入 MOSS 的 SQLite、`settings.json` 与日志。
- 设备在线：自建模式下每 5 秒请求接入点 `…/mcp_endpoint/health?key=…`（3 秒超时），取 `robot_connections`；未配置或失败时为未知。

## 4c. 内置语音服务（NV，仅桌面端，建设中）

- 方案见 [MOSS 内置语音服务技术书](../integration/moss-native-voice-spec.md)。小智设备直接连到 MOSS，不再需要 xiaozhi-server 与接入点。
- `crates/moss-xiaozhi-proto`：纯协议（消息、帧协议 v1/v2/v3、设备 token、OTA）。`crates/moss-voice-server`：专用 tokio 运行时（2 个工作线程）+ axum，OTA（默认 8003）与设备 WebSocket（默认 8000）两个端口，监听所有网卡但只提供 `/xiaozhi/*`，不暴露 MOSS API；每台设备一个会话。
- 只在用户在语音中心点“启动”时创建运行时，停止时整个运行时关闭（最多等 2 秒）；不影响现有 HTTP 服务、发送线程与急停。
- 设备登记 `voice_devices`（schema v6）与 token 密钥（`app_secrets`）由桌面壳的 `DbRegistry` 提供（`apps/desktop/src-tauri/src/voice_builtin.rs`）；未允许、已撤销或 token 无效的设备在握手后收到 `alert` 提示并断开。
- 音频（NV2，`moss-voice-server/src/audio/`）：上行 Opus 解码（opus2 纯 Rust 后端 mousiki，不需要 CMake）；自动拾音模式用 `Vad` 接口判停（当前为进程内双阈值能量检测，静音 200 ms 判停；Silero 随 NV6 本地模型辅助程序接入同一接口）；下行 24 kHz，经线性重采样与 `PlaybackPump` 编码，先连发 3 帧、之后按绝对时钟每 60 ms 一帧，供不上时补静音帧；设备 `abort` 或新的 `listen start` 立即停播并丢弃未播内容。
- 对话模式（NV3，`moss-voice-server/src/conversation.rs` + `crates/moss-voice-providers`）：每轮对话一个 tokio 任务——识别 → 大模型（OpenAI 兼容，流式，可调用工具，最多 5 轮）→ 切句 → 逐句合成；每句音频带文字标记进节奏泵，播到时发 `sentence_start`；打断即中止任务。历史每台设备一份，按长度截断，随会话结束丢弃。唤醒问候、退出词与空闲再见。提供方错误不含地址与密钥。
- 工具（NV4）：MOSS 工具经 `moss_voice_server::HostTools`（宿主实现 `ToolHost`，桌面端调用 `laser_host::voice_tool_call`，身份 `CallerContext::Voice`）；`approval_required` 的结果直接朗读固定话术，不交给模型改写。设备声明 `features.mcp` 时，会话经同一 WebSocket 做 `initialize`、分页 `tools/list`，设备工具以 `device__<名称>` 提供给模型（`.` 等字符换成 `_`，调用时换回原名）。
- 本地模型（NV6）：`apps/moss-voice-local`（GPL-3.0，独立工作区）运行 sherpa-onnx 的判停（Silero / TEN-VAD）、识别（SenseVoice 整段 / X-ASR 流式）、合成（Kokoro / Matcha），用户在语音中心每类选一个；MOSS 经标准输入输出（`crates/moss-voice-local-proto`）调用，客户端 `moss_voice_providers::local`（流式识别模型经 `LocalStreamingAsr` 接入边说边识别）；判停经 `moss_voice_server::VadFactory` / `LocalVad` 接入，出错退回能量判停。模型由 `moss_voice::local_models` 下载到运行目录 `models/` 并校验 SHA-256。辅助程序与 `moss-desktop.exe` 同目录，崩溃不影响 MOSS 主进程与急停。
- 流式云厂商（NV7，`moss_voice_providers::doubao`）：豆包流式识别实现 `StreamingAsr`——会话在开始说话时建立 WebSocket、边说边送 16 kHz 音频，说完只等最后结果（失败退回整段识别）；豆包双向流式合成实现 `Tts::synthesize_stream`——每句第一段音频到达即播放，连接在句与句之间复用。帧协议移植自 univoice（MIT），WebSocket 与 axum 共用 tokio-tungstenite 0.29，TLS 为 rustls。
- 接入点增强（NV8）：小智桥每个接入点一条线程（主接入点 `mcp_endpoint` + `mcp_extra_endpoints` 最多 4 个，自建模式下附加接入点暂停），30 秒心跳、90 秒无数据判定断线，遥测按接入点记录状态（`McpTelemetrySnapshot::endpoints`）。工具开关 `mcp_disabled_tools` 由 `ApiService` 持有，经 `HostBackend::tool_disabled` 在 `tools/list` 与调用两处生效，接入点与内置语音共用；`laser_safe_action_tool` 不可关闭。`dispatch_tool_call_with` 统一计入调用统计（`tool_stats`，内存）。
- 桌面端（NV5，`apps/desktop/src-tauri/src/voice_builtin.rs`）：模型地址等存 `settings.json` 的 `voice.builtin_models`，密钥存 `app_secrets`（`voice.llm.main` / `voice.asr.main` / `voice.tts.main`）；三项都配置时以对话模式启动（OpenAI 兼容提供方 + `DesktopToolHost` 调用 `laser_host::voice_tool_call`），否则回声模式。语音链路 `builtin`（`moss_voice::VoiceMode::Builtin`）时随 MOSS 后台启动；语音中心切换链路经 `HostHooks::builtin_start / builtin_stop`，启动失败整体回滚。设备会话数经 `DeviceRegistry::sessions_changed` 写入 `global_voice_telemetry`，`/api/system/info` 的 `voice.mode` 为 `builtin` 时据此报告。

## 5. 设备层与发送链路

传输只有两种：`ConnectionSpec::Serial`（`serialport`）与 `ConnectionSpec::Tcp`（Telnet，默认 23）。没有 HTTP `/command` 通道，也没有 WebSocket 设备通道。

一次完整发送（workflow `confirm_send` 或材料矩阵 `send`）按以下顺序执行，任一步失败都不写入加工指令：

1. `authorize_send`：记录状态必须为 `pending`，重新读取 G-code 文件计算 SHA-256 并与冻结值比对，且存在有效 workflow 快照或 calibration SHA。
2. 同一设备已有在途作业则返回 409 `device_busy`。
3. 打开传输；失败返回 `device_unreachable`。
4. `handshake_with_deadline`（3 秒）：发送 `$$`，消费到终止 `ok`，要求 `$32=1`，记录 `$30` 与 `$10`；失败返回 `device_not_ready`。
5. `load_features`：发送 `$CD` 读取 FluidNC YAML（行程、`speed_map`、宏、急停/故障/门引脚、`planner_blocks`），旧 GRBL 返回 `error:3` 时静默跳过。
6. `verify_job_ready_adaptive`：`$30` 必须为有限正数；发送 `?` 要求 Idle，拒绝 Run/Jog/Home/Hold/Door/Sleep/Check/Alarm、SD 运行标记以及 `Pn:` 含 `E`/`F`/`D`；失败返回 `device_preflight_failed`。换算系数为 `$30 / 1000`。
7. 设备预检后重新读取物理文件并与确认快照 SHA-256 比对；不一致返回 `send_gate_rejected`，不发送加工行。`prepare_gcode_lines` 只处理这份已验证的内存内容：剥离 `;` 与 `( )` 注释、丢弃空行，单行超过 254 字节或含非 ASCII 即拒绝整个文件（`gcode_line_invalid`），空文件拒绝（`gcode_empty`）。SHA 门禁校验的仍是原文件。
8. `JobWorker` 逐行流式发送：字符计数流控，网络初始窗口 1024 字节、串口 255 字节，收到 `|Bf:` 后按固件报告自适应；S 值按第 6 步系数在线换算；`lines_sent` 只统计原文件行。
9. 全部 ACK 后发送 `G4 P0` 同步规划器，再查询 Idle，通过才写 `completed`；中间状态为 `awaiting_completion`。
10. SD 脱机模式（`offline_sd=true` 或 `send_mode=sd_offline`）：已验证的内存文件经 FluidNC HTTP `/upload` 写入 SD 卡，用 `$File/ShowHash` 比对固件哈希，再 `$SD/Run`，进度来自状态报告的 `SD:` 字段；固件未启用 HashFS、拿不到文件哈希时拒绝 `$SD/Run`（2026-10-14 起）。

取消与急停：`0x18` 通过 `RealtimeCommand::EmergencyStop` 走实时通道，不排在 G-code 队列之后；进程退出的五条路径（前端退出命令、托盘退出、窗口关闭、panic、控制台关闭或 Unix 信号）都经 `ShutdownCoordinator::trigger_shutdown` 调用 `abort_all_active_jobs`，先置取消再写 `0x18`。`0x18` 是软件复位请求，不替代硬件急停。

## 6. 存储与运行目录

- 运行目录由 `RuntimeRoot::resolve` 决定：显式参数 → 可执行文件同级 `.runtime/` → 系统用户数据目录（Windows `%APPDATA%\moss`、macOS `~/Library/Application Support/moss`、Linux `$XDG_DATA_HOME/moss` 或 `~/.local/share/moss`）→ 当前目录 `.runtime/`。宿主与桌面端都传 `(None, None)`，因此各桌面平台均写入当前用户数据目录；程序目录只放程序文件，安装与卸载规则见 [安装包与卸载规范](packaging-install.md)。目录内有 `uploads/`、`previews/`、`generated_assets/`、`moss.sqlite`、`settings.json`、`moss.lock`（`fs4` 独占锁，第二个实例初始化失败）。
- SQLite 打开时启用 WAL、5 秒 `busy_timeout`、`synchronous=NORMAL`，并执行 schema 迁移（当前版本 6，`laser_runtime::SCHEMA_VERSION`）。启动时 `recover_after_restart` 把残留的 `running/pending/queued` job 与 `sending/processing` 的 workflow、标定矩阵标为 `failed`（列与记录正文同时更新）；随后审批服务处理遗留审批（§4a）。
- `RuntimeStore` 只有 `get` / `put` / `set_status` / `list_recent` 四个方法，作用于 `Workflow`、`TextTask`、`Job`、`Calibration` 四种记录；材料库通过 `RuntimeDb::import_material_json` / `export_material_library` 读写。运行时实际写入的记录类型是 workflow 与 calibration；`approvals` 与 `audit_log` 由审批服务写入；`jobs`、`auth_pairings`、`ai_state`、`gui_calibration` 表已建但当前没有运行时写入方，`uploads` 与 `jobs` 只由迁移器写入。表结构见 [数据库结构](db-schema.md)。
- 材料库：`LASER_MATERIAL_PARAMS_FILE` 未设置时使用 `builtin_material_library`，保存操作写入 SQLite；设置后另写该 JSON 文件并生成 `.bak` 备份。

## 7. 易误解的实现事实

以下各项与早期规划或常见假设不同，以本节为准：

- 仓库根目录现在有 `Cargo.toml` 并声明统一 workspace，成员包括 `apps/moss-host`、`apps/desktop/src-tauri` 与 `crates/*`；根级 `Cargo.lock` 和 `target/` 承载 workspace 的统一测试与构建。`apps/desktop/src-tauri` 仍保留 Tauri 子工程所需的本地锁文件与生成目录。`AGENTS.md`、`README.md`、CI 与 `scripts/start.bat` 中的 `cargo test --workspace`、`cargo build -p …` 均以根 workspace 清单为准。
- 没有 tokio、axum、tower、clap、tracing、uuid、ab_glyph 或 rmcp；HTTP 服务与 MCP 桥为同步实现。`laser-api` 的 Jev/OpenRouter 反馈客户端使用阻塞式 `reqwest`，判断结果只用于建议，失败时回退本地规则，不直接控制设备；前端 E2E 使用 Playwright。
- `/v1/events` 不是持续事件流；没有 WebSocket 服务端。
- `laser-api` 直接依赖 `rusqlite`（规格要求禁止）。
- `laser-tools` 是单文件 crate，不是 16 个模块；`apps/mobile/` 不存在，Android 工程在 `apps/desktop/src-tauri/gen/android`。
- `moss-host` 只有 `serve` 与 `migrate` 两个子命令。
- 没有 `MOSS_RUNTIME_DIR` 环境变量，也没有 `.runtime/auth/host_key`；配对 token 不落库。
- 发送前不再要求 `$30=1000`，而是按设备实际 `$30` 在线换算 S 值。
- `laser_core::safety::evaluate_material` 已由 `laser-api` 接入文字、图片、G-code workflow 预览，以及 workflow / 标定矩阵发送和断点恢复；PVC 与明确危险材料返回 `material_safety_blocked`，成分不明塑料要求澄清。小智提示词仍负责对话层确认与实物复核，不能替代后端门禁。
- Excalidraw 字体没有自托管到 `frontend/public/`，由 Vite 打进 `dist/assets`。
