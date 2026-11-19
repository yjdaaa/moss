# 架构总览

本文描述当前仓库结构，依据根级 workspace、各 `Cargo.toml`、源码与测试整理。

## 1. 进程与入口

| 进程 | 位置 | 说明 |
| --- | --- | --- |
| `moss-desktop` | `apps/desktop/src-tauri` | Tauri 2 外壳。`run()` 先安装 panic 钩子与控制台关闭钩子，再在进程内初始化 `laser_host::HostApplication`，启动 HTTP 服务与 MCP 桥，最后创建窗口。桌面平台带系统托盘；关闭窗口被拦截并通知前端弹出"托盘 / 退出"选择。Android/iOS 通过 `mobile_entry_point` 复用同一份代码，前端资源来自 `frontend/dist` |
| `moss-host` | `apps/moss-host` | 命令行宿主。`serve` 启动 HTTP 服务与 MCP 桥并阻塞；`migrate --from <旧根>` 迁移旧 Python 版运行数据；无参数时只跑三项启动自检 |
| 前端 | `frontend/` | React 18 + Vite 6 + Excalidraw 0.18。路由 `/`、`/draw`、`/twin`、`/material-lab`（桌面端另有 `/camera`、`/voice`）由 `main.jsx` 按 `pathname` 前缀切换。生产资源由 `laser-api` 通过 `rust-embed` 嵌入并从 `/`、`/assets/*` 提供；Tauri 内嵌页启动时用 `get_backend_origin` 获取实际回环端口（失败则显示错误），桌面同源页使用当前地址；独立 Vite 开发页仍回退到 `http://127.0.0.1:9666` |

## 2. crate 分层

依赖只允许向下，激光域与语音域互不依赖，只经边界层的类型或编排层注入的接口往来。`scripts/check-crate-layers.mjs` 直接解析各 `Cargo.toml` 把关（`scripts/tests/crate-layers.test.mjs` 随 CI 运行）；测试依赖可以跨域，但任何依赖都不得指向应用壳。新增 crate 必须先在脚本的 `LAYERS` 登记。

| 层 | crate | 可依赖 |
| --- | --- | --- |
| 0 边界与通用库 | `laser-contracts`、`moss-xiaozhi-proto`、`moss-voice-local-proto`、`moss-mcp-client`、`moss-process` | 仓库内无 |
| 1 共享运行时 | `laser-runtime`（运行目录、配置、SQLite 与各类存储、密钥，激光与语音共用） | 第 0 层 |
| 2 激光域 | `laser-core`、`laser-device`、`laser-tools`、`laser-mcp`、`laser-api` | 第 0、1 层与本域 |
| 2 语音域 | `moss-voice`、`moss-voice-providers`、`moss-xiaozhi-display`、`moss-voice-server`、`moss-usb-provision` | 第 0、1 层与本域 |
| 3 编排与跨域扩展 | `laser-host`（启动组装）；`moss-agent`、`moss-home`（同时用到激光审批与语音设置的扩展，彼此不互相依赖） | 以下各层；本层之间不互相依赖 |
| 4 宿主总装配 | `moss-voice-host`（聚合激光宿主、内置语音、长期记忆与跨域扩展，零 Tauri/UI 依赖） | 以下各层 |
| 5 应用壳 | `moss-host`、`moss-desktop`、`moss-voice-local`（独立工作区） | 以下各层；不得被依赖 |

`crates/laser-contracts/tests/type_boundary.rs` 随测试运行，禁止 `laser-tools`、`laser-device`、`laser-core` 依赖 `serde_json` 或使用 `Value` 动态访问器，所有跨 crate 数据必须经 `laser-contracts` 的强类型结构。

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
| `laser-api` | HTTP 服务与业务编排：所有 `/api/*`、`/v1/*` 路由（按系统/设备/工作流/材料拆分子模块并暴露进程内类型化入口 `dispatch_json`），workflow 状态机，发送门禁与 `JobWorker` 调度，材料实验室，局域网配对，桌面会话、严格模式与桌面审批（`approval.rs`），前端静态资源 | `ApiService`、`dispatch_json`、`ShortJobWatch`、`AuthManager`、`abort_all_active_jobs`、`ApprovalService`、`require_r3_authority` |
| `laser-host` | 组装层：初始化运行目录、配置、数据库、`ApiService`，启动 HTTP 与 MCP 桥，全局退出协调器，语音工具分发与第三方 MCP 接线（`external_mcp`） | `HostApplication`、`InProcessBackend`、`ShutdownCoordinator`、`voice_tool_call`、`external_mcp::ExternalMcp` |
| `moss-agent` | 本机 Agent（NV13，Claude Code / Codex）：CLI 识别与参数校验、Windows Job Object / Unix 进程组受限运行器、桌面单次审批桥 | `AgentCoordinator`、`AgentRunner`、`detect_cli`、`Provider` |
| `moss-home` | 家居控制与只读发现（NV15，Home Assistant）：只读实体发现（`home_readonly`）、历史备份只读审计（`home_backup_audit`）、实体白名单策略、逐次桌面审批、HTTPS 传输与结果核验；只持宿主弱引用 | `HomeControlCoordinator`、`home_readonly`、`home_backup_audit` |
| `moss-process` | 第 0 层跨平台子进程生命周期守卫：Windows Job Object（`KILL_ON_JOB_CLOSE`）进程树托管句柄，供 OneJev / 本地视觉 / Laya / `moss-voice-local` / `speech.cpp` 共用 | `JobHandle` |
| `moss-usb-provision` | 小智 USB 一键配网：串口枚举与激光串口避让、`espflash` 读写 NVS、写后芯片侧 MD5 校验与失败写回、预签设备令牌并登记“已允许”；激光占用、设备密钥由桌面壳传入 | `provision`、`execute_usb_provision_with`、`UsbFlasherSession`、`sweep_usb_temp_dir` |
| `moss-xiaozhi-display` | 小智彩屏渲染、护目镜小机器人动图、中文字库与 `glyph_push` 动态字模补齐、全屏画风与唤醒词 `assets.bin` 纯构造（承载 4.8 MB 字库与 3.9 MB ESP-SR 模型，零网络与异步依赖） | `glyph_font::GlyphFontService`、`goggle_bot`、`render`、`screen_pack`、`wake_pack` |
| `moss-voice-host` | 内置语音与长期记忆宿主总装配（零 Tauri/UI 依赖）：`BuiltinVoice` 生命周期、设备登记、本地模型/Colab/视觉/家居/声纹/固件/唤醒词/USB 配网编排、`ContextHost` 长期记忆与资料库 | `BuiltinVoice`、`BuiltinState`、`init`、`autostart`、`record_moss_event`、`context::ContextHost` |

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
- 需要持久化或真实作业的工具（workflow、材料库、标定矩阵、文字任务、正式发送）通过 `HostBackend` 直接调用进程内 `ApiService::dispatch_json` / `dispatch_json_with_watch`（跳过原始 HTTP/1.1 文本报文拼装与头解析，走同一鉴权后门禁与状态机，按预览走边框通过显式 `ShortJobWatch` 条件变量同步开始回执），与网页共用同一套门禁、SQLite 与 `JobWorker`。`laser_grbl_tool` / `laser_network_grbl_tool` 的 `send_file`、`send_command` 直接拒绝并引导走 workflow 或安全动作。
- 桥的连接状态、客户端身份与最近请求写入 `laser_contracts::global_mcp_telemetry`，由 `/api/system/info` 暴露给前端。

## 4a. 严格模式与桌面审批（P1）

- 开关 `MOSS_FUSION_STRICT`（默认 `false`）。关闭时下述检查一律不生效，行为与之前一致；`MOSS_GATEWAY_ENABLED=true` 而严格模式关闭时宿主拒绝启动。
- 唯一检查点 `ApiService::require_r3_authority`，接在全部 R3 执行入口：`launch_send_job_for`（workflow / 文字任务 / 标定矩阵发送与断点续雕）、`/api/safe-action` 的 R3 动作、`/api/device/framing`。桌面会话放行；小智桥接 / 局域网 / 网关创建审批并返回 202 `approval_required`；本机回环无凭据返回 403 `strict_mode_blocked`；审批身份按当前要素与保存的连接重算摘要并单次消费，执行使用审批保存的连接参数。
- `ApprovalService`（`crates/laser-api/src/approval.rs`，严格模式检查、桌面会话与重启处理在 `approval/` 子模块）管理审批生命周期，数据在 SQLite `approvals` 表（schema v4/v5）；创建、批准、执行与审计在同一事务中，审计写不进去就拒绝。经审批的发送在启动前记 `phase=running`，作业结束由发送线程回写；进程重启时待批准 / 已批准的过期，仍在发送的记为结果未知，均不重放。
- 桌面审批接口 `/v1/approvals*` 只接受桌面会话；会话值由 Tauri 壳启动时生成（`get_desktop_session`），独立 `moss-host serve` 没有桌面会话，严格模式下只能拒绝或等待过期。
- 小智接入点按 `laser_contracts::tool_risk` 分级：严格模式下 R4 与未分级工具直接拒绝，R3 交给执行层生成审批。前端全局审批卡见 [前端行为契约](frontend-behavior.md) §8。

## 4b. 语音中心（P4，仅桌面端）

- crate `crates/moss-voice` 不依赖 Tauri，由 `apps/desktop/src-tauri` 在非 Android / iOS 目标引入（`src/voice.rs`，Tauri 命令见 [接口契约](api-contracts.md) §5）；`laser-host` 的 `external_mcp`（同样限非移动目标）与 `moss-agent`、`moss-home`、`moss-usb-provision` 只用其设置读写；`laser-api`、`moss-host`、Android 不包含进程管理代码。
- `VoiceCenter` 编排页面操作；MOSS 后端的能力经 `HostHooks` 交入：严格模式状态（只读展示；开关由 `voice_strict_set` 写 `.env` 并热切换）、小智桥重连（`HostApplication::restart_mcp_bridge`：先停旧桥、等待其退出，再按 `settings.json` 接新桥）、桥连接状态、设备会话数（切到官方时清零；内置链路由内置服务写 `laser_contracts::global_voice_telemetry`，`/api/system/info` 的 `voice.device_sessions` 读取）、内置服务启停。
- 链路切换：官方 / 内置二选一，切换不改 `mcp_endpoint` 与严格模式，只写 `settings.json` 的 `voice.mode` 并启停内置服务，失败恢复设置与内置服务。2026-11-17 起自建服务端链路连代码删除（原 `supervisor` 子进程管理、`config_yaml` 模型向导、`health` 接入点健康检查、服务端日志与“自建链路强制严格模式”一并移除）。旧设置里的 `voice.mode = "self_hosted"` 读作 `builtin`，桌面端在后端初始化后、启动小智桥前调用 `moss_voice::settings::migrate_legacy_self_hosted` 一次性改写（有官方接入点备份则写回顶层 `mcp_endpoint`）。
- 子进程句柄：内置服务的全部本地子进程（OneJev、本地视觉、Laya、`moss-voice-local`、`speech.cpp`）统一共用 `moss_process::JobHandle`（Windows Job Object `KILL_ON_JOB_CLOSE`，随 MOSS 关闭或异常退出结束进程树）。
- 退出顺序：`ShutdownState::request()`（急停与任务清零）→ 停止内置语音服务与第三方 MCP 子进程 → 退出。

## 4c. 内置语音服务（NV，仅桌面端，建设中）

- 方案见 [MOSS 内置语音服务技术书](../integration/moss-native-voice-spec.md)。小智设备直接连到 MOSS，不再需要 xiaozhi-server 与接入点。
- `crates/moss-xiaozhi-proto`：纯协议（消息、帧协议 v1/v2/v3、设备 token、OTA、ESP-IDF 分区表与 NVS v2 编解码 `nvs`）。`crates/moss-voice-server`：专用 tokio 运行时（2 个工作线程）+ axum，OTA（默认 8003）与设备 WebSocket（默认 8000）两个端口，监听所有网卡但只提供 `/xiaozhi/*`，不暴露 MOSS API；每台设备一个会话。
- 只在用户在语音中心点“启动”时创建运行时，停止时整个运行时关闭（最多等 2 秒）；不影响现有 HTTP 服务、发送线程与急停。
- 设备登记 `voice_devices`（schema v6）与 token 密钥（`app_secrets`）由 `moss-voice-host` 的 `DbRegistry` 提供（`crates/moss-voice-host/src/builtin/devices.rs`）；未允许、已撤销或 token 无效的设备在握手后收到 `alert` 提示并断开。
- 音频（NV2，`moss-voice-server/src/audio/`）：上行 Opus 解码（opus2 + 官方 libopus，随源码编译，构建需要 CMake；n94 起，原纯 Rust 后端 mousiki 在语音模式下与设备不兼容）；自动拾音模式用 `Vad` 接口判停（当前为进程内双阈值能量检测，静音 250 ms 即交给识别与大模型抢跑，开口前按句子是否说完再等 0–1 秒（技术书 n86）；Silero 随 NV6 本地模型辅助程序接入同一接口）；下行 24 kHz，经线性重采样与 `PlaybackPump` 编码，`tts start` 后留 180 ms 保护间隔，再按设备端缓冲余量（目标 5 帧）发送、稳态每 60 ms 一帧，供不上时不发帧；设备 `abort` 或新的 `listen start` 立即停播并丢弃未播内容。
- 对话模式（NV3，`moss-voice-server/src/conversation.rs` + `crates/moss-voice-providers`）：每轮对话一个 tokio 任务——识别 → 大模型（OpenAI 兼容，流式，可调用工具，最多 5 轮）→ 切句 → 逐句合成；每句音频带文字标记进节奏泵，播到时发 `sentence_start`；打断即中止任务。历史每台设备一份，按长度截断，随会话结束丢弃。唤醒问候、退出词与空闲再见。提供方错误不含地址与密钥。
- 工具（NV4）：MOSS 工具经 `moss_voice_server::HostTools`（宿主实现 `ToolHost`，桌面端调用 `laser_host::voice_tool_call`，身份 `CallerContext::Voice`）；`approval_required` 的结果直接朗读固定话术，不交给模型改写。设备声明 `features.mcp` 时，会话经同一 WebSocket 做 `initialize`、分页 `tools/list`，设备工具以 `device__<名称>` 提供给模型（`.` 等字符换成 `_`，调用时换回原名）。
- 本地模型（NV6 / n93）：`apps/moss-voice-local`（GPL-3.0，独立工作区）运行 sherpa-onnx 的判停（Silero / TEN-VAD）、前端降噪（GTCRN，`denoise` 默认 `false`）、命令词抗噪（关键词检测 zipformer，随判停流运行，`kws` 默认 `false`，协议 7 / n101）、识别（SenseVoice / Qwen3-ASR / FireRedASR2-CTC 约 0.5 秒周期增量重认 + 整段 / X-ASR 原生流式）、合成（Kokoro / Matcha / ZipVoice-Distill），另外 Qwen3-TTS 0.6B（`qwen3_tts_0_6b`）经按需下载且 SHA-256 校验的独立 `speech.cpp v0.10.1` 进程运行（`moss_voice::speech_cpp` / `moss_voice_providers::speech_cpp`），SmartTurn v3 尾音语义判停（`turn` 默认 `false`）由独立 `TurnHelper` 进程（单次 250 ms 软超时）与 `moss_voice_server::TurnEndPredictor` 并发估算且永不延迟 `is_stop` / `TurnGate`；MOSS 经标准输入输出协议 6（`crates/moss-voice-local-proto`）调用，客户端 `moss_voice_providers::local`（`streaming` 支持的本地识别均经 `LocalStreamingAsr` 接入边说边识别，`LocalAsr` 作整段兜底）；判停经 `moss_voice_server::VadFactory` / `LocalVad` 接入，出错退回能量判停。模型由 `moss_voice::local_models`（`CATALOG` 26 项）下载到运行目录 `models/` 并校验 SHA-256。辅助程序与 `moss-desktop.exe` 同目录，崩溃不影响 MOSS 主进程与急停。
- 流式云厂商（NV7，`moss_voice_providers::doubao`）：豆包流式识别实现 `StreamingAsr`——会话在开始说话时建立 WebSocket、边说边送 16 kHz 音频，说完只等最后结果（失败退回整段识别）；豆包双向流式合成实现 `Tts::synthesize_stream`——每句第一段音频到达即播放，连接在句与句之间复用。帧协议移植自 univoice（MIT），WebSocket 与 axum 共用 tokio-tungstenite 0.29，TLS 为 rustls。
- 接入点增强（NV8）：小智桥每个接入点一条线程（主接入点 `mcp_endpoint` + `mcp_extra_endpoints` 最多 4 个），30 秒心跳、90 秒无数据判定断线，遥测按接入点记录状态（`McpTelemetrySnapshot::endpoints`）。工具开关 `mcp_disabled_tools` 由 `ApiService` 持有，经 `HostBackend::tool_disabled` 在 `tools/list` 与调用两处生效，接入点与内置语音共用；`laser_safe_action_tool` 不可关闭。`dispatch_tool_call_with` 统一计入调用统计（`tool_stats`，内存）。
- 桌面端（NV5，`apps/desktop/src-tauri/src/voice_builtin.rs` 与 `voice_context.rs` 仅保留 Tauri 命令、对话框与主窗口来源校验，零 UI 编排主体下沉至 `crates/moss-voice-host`，其中 `builtin/usb_provision.rs` 只做 `voice_usb_provision` 的并发锁、激光串口判断与诊断记录，串口读/写小智 `0x9000` NVS 分区与预签设备密钥在 `crates/moss-usb-provision`）：模型地址等存 `settings.json` 的 `voice.builtin_models`，密钥存 `app_secrets`（`voice.llm.main` / `voice.asr.main` / `voice.tts.main`）；三项都配置时以对话模式启动（OpenAI 兼容提供方 + `DesktopToolHost` 调用 `laser_host::voice_tool_call`），否则回声模式。语音链路 `builtin`（`moss_voice::VoiceMode::Builtin`）时随 MOSS 后台启动；语音中心切换链路经 `HostHooks::builtin_start / builtin_stop`，启动失败整体回滚。设备会话数经 `DeviceRegistry::sessions_changed` 写入 `global_voice_telemetry`，`/api/system/info` 的 `voice.mode` 为 `builtin` 时据此报告。

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
8. `JobWorker` 逐行流式发送：字符计数流控（2026-11-05 起）：`$CD` 确认为 FluidNC 的网络连接起步 256 字节，串口与未知设备按经典 GRBL 起步 127 字节；无未确认命令时观测到的 `|Bf:` 剩余值即真实容量，窗口取观测最大值 − 1（上限 1024），发送中的 `Bf:` 只会抬高、不压低，绝不套高于设备报告值的下限；`$RI` 自动上报设置超时会补发 `G4 P0` 对齐应答，对不齐则不发任何刀路行；S 值按第 6 步系数在线换算；`lines_sent` 只统计原文件行。
9. 全部 ACK 后发送 `G4 P0` 同步规划器，再查询 Idle，通过才写 `completed`；中间状态为 `awaiting_completion`。
10. SD 脱机模式（`offline_sd=true` 或 `send_mode=sd_offline`）：已验证的内存文件经 FluidNC HTTP `/upload` 写入 SD 卡，用 `$File/ShowHash` 比对固件哈希，再 `$SD/Run`；设备 `$30` 不是 1000 时，上传内容与流式发送相同（已校验行按标尺换算 S 值），固件哈希比对实际上传字节的 SHA-256，原文件 SHA 仍先与确认快照比对（2026-11-05 起），进度来自状态报告的 `SD:` 字段；固件未启用 HashFS、拿不到文件哈希时拒绝 `$SD/Run`（2026-10-14 起）。

取消与急停：`0x18` 通过 `RealtimeCommand::EmergencyStop` 走实时通道，不排在 G-code 队列之后；进程退出的五条路径（前端退出命令、托盘退出、窗口关闭、panic、控制台关闭或 Unix 信号）都经 `ShutdownCoordinator::trigger_shutdown` 调用 `abort_all_active_jobs`，先置取消再写 `0x18`。`0x18` 是软件复位请求，不替代硬件急停。

## 6. 存储与运行目录

- 运行目录由 `RuntimeRoot::resolve` 决定：显式参数 → 可执行文件同级 `.runtime/` → 系统用户数据目录（Windows `%APPDATA%\moss`、macOS `~/Library/Application Support/moss`、Linux `$XDG_DATA_HOME/moss` 或 `~/.local/share/moss`）→ 当前目录 `.runtime/`。宿主与桌面端都传 `(None, None)`，因此各桌面平台均写入当前用户数据目录；程序目录只放程序文件，安装与卸载规则见 [安装包与卸载规范](packaging-install.md)。目录内有 `uploads/`、`previews/`、`generated_assets/`、`moss.sqlite`、`settings.json`、`moss.lock`（`fs4` 独占锁，第二个实例初始化失败）。
- SQLite 打开时启用 WAL、5 秒 `busy_timeout`、`synchronous=NORMAL`、`secure_delete=ON`，并执行 schema 迁移（当前版本 20，`laser_runtime::SCHEMA_VERSION`）；文件库下另由 `RuntimeDb::open_readonly` 打开独立只读连接分流高频轮询。启动时 `recover_after_restart` 把残留活动态记录标为 `failed`（列与记录正文同时更新），`prune_historical_records` 定额清理超上限旧记录，`prune_orphan_generated_files` 清理 `generated_assets/` 与 `previews/` 内超 7 天且无数据库引用的自动生成孤儿产物（保留 `uploads/`）；随后审批服务处理遗留审批（§4a）。退出时 `abort_active_records_on_shutdown` 同步更新列与正文状态，并执行 `PRAGMA optimize` 与限时 WAL 截断归并。
- `RuntimeStore` 作用于 `Workflow`、`TextTask`、`Job`、`Calibration` 四种记录（生产环境经 `SplitSqliteStore` 读写分流）；材料库通过 `RuntimeDb::import_material_json` / `export_material_library`（固定 3 次预编译查询）读写。运行时实际写入的记录类型是 workflow 与 calibration；`approvals` 与 `audit_log` 由审批服务写入；`uploads`、`jobs`、`text_tasks` 只由迁移器写入（7 张从未使用过的旧表 `ai_state`、`gui_calibration`、`auth_pairings` 及 4 张通用 `_history` 已在 schema v18 删除）。表结构见 [数据库结构](db-schema.md)。
- 材料库：`LASER_MATERIAL_PARAMS_FILE` 未设置时使用 `builtin_material_library`，保存操作写入 SQLite；设置后另写该 JSON 文件并生成 `.bak` 备份。

## 7. 易误解的实现事实

以下各项与早期规划或常见假设不同，以本节为准：

- 仓库根目录现在有 `Cargo.toml` 并声明统一 workspace，成员包括 `apps/moss-host`、`apps/desktop/src-tauri` 与 `crates/*`；根级 `Cargo.lock` 和 `target/` 承载 workspace 的统一测试与构建（仅独立 GPL 工作区 `apps/moss-voice-local` 保留独立 `Cargo.lock`，其余子目录不再保留陈旧锁文件；`apps/desktop/src-tauri` 保留 `gen/android` 等 Tauri 子工程生成骨架）。`AGENTS.md`、`README.md`、CI 与 `scripts/start.bat` 中的 `cargo test --workspace`、`cargo build -p …` 均以根 workspace 清单为准。
- 激光主链路（`laser-api` HTTP 服务与 `laser-mcp` 桥）不用 tokio、axum、tower、clap、tracing、uuid、ab_glyph 或 rmcp，为同步实现；tokio 用于内置语音（`moss-voice-server`、`moss-voice-providers`）、`moss-agent`、`moss-mcp-client` 与 `laser-host::external_mcp`，axum 只在 `moss-voice-server`（设备 OTA / WebSocket），rmcp 只在 `moss-mcp-client`。`laser-api` 的 Jev/OpenRouter 反馈客户端使用阻塞式 `reqwest`，判断结果只用于建议，失败时回退本地规则，不直接控制设备；前端 E2E 使用 Playwright。
- `/v1/events` 不是持续事件流；`laser-api` 没有 WebSocket 服务端（小智设备 WebSocket 只在内置语音服务，§4c）。
- `laser-api` 及其他业务 crate 均不直接依赖 `rusqlite`，`RuntimeDb.connection` 收口为 `pub(crate)`，所有 SQLite 读写与事务统一经 `laser-runtime::RuntimeDb` 接口完成（并由 `scripts/check-crate-layers.mjs` 强制把关）。
- `laser-tools` 是单文件 crate，不是 16 个模块；`apps/mobile/` 不存在，Android 工程在 `apps/desktop/src-tauri/gen/android`。
- `moss-host` 只有 `serve` 与 `migrate` 两个子命令。
- 没有 `MOSS_RUNTIME_DIR` 环境变量，也没有 `.runtime/auth/host_key`；配对 token 不落库。
- 发送前不再要求 `$30=1000`，而是按设备实际 `$30` 在线换算 S 值。
- `laser_core::safety::evaluate_material` 已由 `laser-api` 接入文字、图片、G-code workflow 预览，以及 workflow / 标定矩阵发送和断点恢复；PVC 与明确危险材料返回 `material_safety_blocked`，成分不明塑料要求澄清。小智提示词仍负责对话层确认与实物复核，不能替代后端门禁。
- Excalidraw 字体没有自托管到 `frontend/public/`，由 Vite 打进 `dist/assets`。
