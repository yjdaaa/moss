# 架构总览

本文描述 2026-09-21 时仓库的真实结构，依据各 `Cargo.toml`、源码与测试整理。它取代已归档的 [重构规格](../archive/rust-rebuild-spec.md) 第二部分作为架构依据；两者的差异见文末。

## 1. 进程与入口

| 进程 | 位置 | 说明 |
| --- | --- | --- |
| `moss-desktop` | `apps/desktop/src-tauri` | Tauri 2 外壳。`run()` 先安装 panic 钩子与控制台关闭钩子，再在进程内初始化 `laser_host::HostApplication`，启动 HTTP 服务与 MCP 桥，最后创建窗口。桌面平台带系统托盘；关闭窗口被拦截并通知前端弹出"托盘 / 退出"选择。Android/iOS 通过 `mobile_entry_point` 复用同一份代码，前端资源来自 `frontend/dist` |
| `moss-host` | `apps/moss-host` | 命令行宿主。`serve` 启动 HTTP 服务与 MCP 桥并阻塞；`migrate --from <旧根>` 迁移旧 Python 版运行数据；无参数时只跑三项启动自检 |
| 前端 | `frontend/` | React 18 + Vite 6 + Excalidraw 0.18。路由 `/`、`/draw`、`/material-lab` 由 `main.jsx` 按 `pathname` 前缀切换。生产资源由 `laser-api` 通过 `rust-embed` 嵌入并从 `/`、`/assets/*` 提供；在 Tauri 或 Vite 开发端口下运行时，前端把 API 基址固定为 `http://127.0.0.1:9666` |

## 2. crate 分层

依赖只允许向下。`crates/laser-contracts/tests/type_boundary.rs` 随测试运行，禁止 `laser-tools`、`laser-device`、`laser-core` 依赖 `serde_json` 或使用 `Value` 动态访问器，所有跨 crate 数据必须经 `laser-contracts` 的强类型结构。

```text
laser-host ──► laser-api ──► laser-tools ──► laser-core ──► laser-contracts
    │              │              │
    │              │              └──► laser-device（无内部依赖）
    │              └──► laser-runtime ──► laser-contracts
    └──► laser-mcp ──► laser-tools
```

| crate | 职责 | 关键符号 |
| --- | --- | --- |
| `laser-contracts` | 纯数据模型与 JSON Schema：材料库、workflow / job / text_task / calibration 记录、工具参数与 `ToolCallResult`、MCP 遥测单例、内置材料库 | `MaterialLibrary`、`WorkflowRecord`、`ToolCallResult`、`builtin_material_library` |
| `laser-core` | 纯算法：字体轮廓（`ttf-parser`）、位图抖动与光栅规划、矢量描边与填充、G-code 生成与时间估算、材料推荐与标定矩阵、参数安全范围 | `gcode::write_raster` / `write_outline` / `generate_framing_gcode`、`raster::dither`、`material::calibration_matrix`、`safety::validate_core_job` / `evaluate_material` |
| `laser-device` | 硬件通信：串口与 TCP 传输、有界握手、FluidNC `$CD` 配置解析、Idle 与引脚预检、字符计数流控与 `Bf:` 自适应窗口、S 值按 `$30` 换算、实时字节、mDNS 发现、SD 卡 HTTP 上传、错误码翻译、`FakeGrbl` 测试桩 | `DeviceTransport`、`ConnectionSpec`、`DeviceConnection`、`JobWorker`、`authorize_send`、`RealtimeCommand`、`discover_controllers` |
| `laser-runtime` | 配置与持久化：`.env` 读取、运行目录解析与实例锁、SQLite schema 与迁移、`RuntimeStore` 抽象、旧数据迁移器 | `LaserSettings`、`RuntimeRoot`、`RuntimeDb`、`RuntimeStore`（`SqliteStore` / `MemoryStore`）、`run_migration` |
| `laser-tools` | MCP 工具目录与不依赖宿主的工具实现：连接检查、安全动作、参数推荐、文字图片、桌面应用与快捷键、开发者命令 | `ToolCatalog`、`execute_check_laser_connection`、`execute_laser_safe_action`、`resolve_connection_spec`、`developer_mode_enabled` |
| `laser-mcp` | JSON-RPC 2.0 MCP 服务：`initialize` / `ping` / `tools/list` / `tools/call`，工具分发，连接小智控制台的 WebSocket 客户端（`tungstenite` + `rustls`，同步实现） | `tools_list_json`、`dispatch_tool_call_with`、`HostBackend`、`run_bridge`、`endpoint_from_env` |
| `laser-api` | HTTP 服务与业务编排：所有 `/api/*`、`/v1/*` 路由，workflow 状态机，发送门禁与 `JobWorker` 调度，材料实验室，局域网配对，前端静态资源 | `ApiService`、`AuthManager`、`abort_all_active_jobs` |
| `laser-host` | 组装层：初始化运行目录、配置、数据库、`ApiService`，启动 HTTP 与 MCP 桥，全局退出协调器 | `HostApplication`、`InProcessBackend`、`ShutdownCoordinator`、`install_panic_hook`、`install_console_ctrl_handler` |

## 3. HTTP 服务

- `ApiService::serve` 在 `std::net::TcpListener` 上手写解析 HTTP/1.1，每个连接一个线程，读超时 10 秒。没有 tokio、axum 或任何异步运行时。
- 首选端口来自 `LASER_WEB_PORT`，否则运行目录 `settings.json` 的 `web_port`，否则 `9666`。绑定失败时向后尝试 50 个端口，再失败则由系统分配；`/api/system/info` 返回 `preferred_port`、`actual_port` 与 `is_fallback`。
- 鉴权：回环地址免鉴权。非回环时，`GET /api/health`、`/v1/capabilities`、`/api/system/info` 与页面/静态资源公开，其余路由要求 `Authorization: Bearer`。配对码由 `POST /v1/auth/challenge`（仅回环）签发，`POST /v1/auth/pair` 换取 token；token 与配对状态只在 `AuthManager` 内存中，进程重启即失效。
- `/v1/events` 是一次性 SSE：返回单条 `event: ready` 后关闭连接。前端不使用它，进度通过轮询 `POST /api/workflow {action:"status"}` 获得。
- 路由清单与字段见 [Web API 契约](api-contracts.md)。

## 4. MCP 桥

- 只有在 `MCP_ENDPOINT` 为 `ws://` 或 `wss://` 且不含 `<` 占位符时才启动；否则静默跳过。
- 桥在独立线程中运行 `laser_mcp::run_bridge`，断线按 1 秒起、60 秒封顶指数退避重连，日志中的 token 脱敏。
- 协议版本 `2024-11-05`，服务名 `YOKO_MCP_SERVER`。`tools/list` 返回 `ToolCatalog` 的 20 个工具；未设置 `MOSS_ENABLE_DEVELOPER_COMMANDS=true` 时不含 `command_execution_tool`，共 19 个。
- 需要持久化或真实作业的工具（workflow、材料库、标定矩阵、文字任务、正式发送）通过 `HostBackend` 转发到进程内 `ApiService`，与网页共用同一套门禁、SQLite 与 `JobWorker`。`laser_grbl_tool` / `laser_network_grbl_tool` 的 `send_file`、`send_command` 直接拒绝并引导走 workflow 或安全动作。
- 桥的连接状态、客户端身份与最近请求写入 `laser_contracts::global_mcp_telemetry`，由 `/api/system/info` 暴露给前端。

## 5. 设备层与发送链路

传输只有两种：`ConnectionSpec::Serial`（`serialport`）与 `ConnectionSpec::Tcp`（Telnet，默认 23）。没有 HTTP `/command` 通道，也没有 WebSocket 设备通道。

一次完整发送（workflow `confirm_send` 或材料矩阵 `send`）按以下顺序执行，任一步失败都不写入加工指令：

1. `authorize_send`：记录状态必须为 `pending`，重新读取 G-code 文件计算 SHA-256 并与冻结值比对，且存在有效 workflow 快照或 calibration SHA。
2. 同一设备已有在途作业则返回 409 `device_busy`。
3. 打开传输；失败返回 `device_unreachable`。
4. `handshake_with_deadline`（3 秒）：发送 `$$`，消费到终止 `ok`，要求 `$32=1`，记录 `$30` 与 `$10`；失败返回 `device_not_ready`。
5. `load_features`：发送 `$CD` 读取 FluidNC YAML（行程、`speed_map`、宏、急停/故障/门引脚、`planner_blocks`），旧 GRBL 返回 `error:3` 时静默跳过。
6. `verify_job_ready_adaptive`：`$30` 必须为有限正数；发送 `?` 要求 Idle，拒绝 Run/Jog/Home/Hold/Door/Sleep/Check/Alarm、SD 运行标记以及 `Pn:` 含 `E`/`F`/`D`；失败返回 `device_preflight_failed`。换算系数为 `$30 / 1000`。
7. `prepare_gcode_lines`：剥离 `;` 与 `( )` 注释、丢弃空行，单行超过 254 字节或含非 ASCII 即拒绝整个文件（`gcode_line_invalid`），空文件拒绝（`gcode_empty`）。SHA 门禁校验的仍是原文件。
8. `JobWorker` 逐行流式发送：字符计数流控，网络初始窗口 1024 字节、串口 255 字节，收到 `|Bf:` 后按固件报告自适应；S 值按第 6 步系数在线换算；`lines_sent` 只统计原文件行。
9. 全部 ACK 后发送 `G4 P0` 同步规划器，再查询 Idle，通过才写 `completed`；中间状态为 `awaiting_completion`。
10. SD 脱机模式（`offline_sd=true` 或 `send_mode=sd_offline`）：文件经 FluidNC HTTP `/upload` 写入 SD 卡，用 `$File/ShowHash` 比对固件哈希，再 `$SD/Run`，进度来自状态报告的 `SD:` 字段；固件未启用 HashFS 时继续执行但标记 `sd_hash_verified=false`。

取消与急停：`0x18` 通过 `RealtimeCommand::EmergencyStop` 走实时通道，不排在 G-code 队列之后；进程退出的五条路径（前端退出命令、托盘退出、窗口关闭、panic、控制台关闭或 Unix 信号）都经 `ShutdownCoordinator::trigger_shutdown` 调用 `abort_all_active_jobs`，先置取消再写 `0x18`。`0x18` 是软件复位请求，不替代硬件急停。

## 6. 存储与运行目录

- 运行目录由 `RuntimeRoot::resolve` 决定：显式参数 → 可执行文件同级 `.runtime/` → `%APPDATA%\moss` → 当前目录 `.runtime/`。宿主与桌面端都传 `(None, None)`，因此实际使用可执行文件同级目录。目录内有 `uploads/`、`previews/`、`generated_assets/`、`moss.sqlite`、`settings.json`、`moss.lock`（`fs4` 独占锁，第二个实例初始化失败）。
- SQLite 打开时启用 WAL、5 秒 `busy_timeout`、`synchronous=NORMAL`，并执行 schema 迁移（当前版本 2）。启动时 `recover_after_restart` 把残留的 `running/pending/queued` job 与 `sending/processing` workflow 标为 `failed`。
- `RuntimeStore` 只有 `get` / `put` / `set_status` / `list_recent` 四个方法，作用于 `Workflow`、`TextTask`、`Job`、`Calibration` 四种记录；材料库通过 `RuntimeDb::import_material_json` / `export_material_library` 读写。运行时实际写入的记录类型是 workflow 与 calibration；`jobs`、`auth_pairings`、`audit_log`、`ai_state`、`gui_calibration` 表已建但当前没有运行时写入方，`uploads` 与 `jobs` 只由迁移器写入。表结构见 [数据库结构](db-schema.md)。
- 材料库：`LASER_MATERIAL_PARAMS_FILE` 未设置时使用 `builtin_material_library`，保存操作写入 SQLite；设置后另写该 JSON 文件并生成 `.bak` 备份。

## 7. 与早期规划稿的已知偏差

以下各项在 [重构规格](../archive/rust-rebuild-spec.md) 中有相反或不同的描述，以本节为准：

- 仓库根目录当前没有 `Cargo.toml`，也没有任何 `[workspace]` 段；每个 crate 与 app 各自持有 `Cargo.lock` 与 `target/`。`AGENTS.md`、`README.md`、CI 与 `scripts/start.bat` 中的 `cargo test --workspace`、`cargo build -p …` 都以根 workspace 清单存在为前提。
- 没有 tokio、axum、tower、reqwest、clap、tracing、uuid、ab_glyph、rmcp 或 playwright；HTTP 与 MCP 客户端都是同步实现，参数解析手写，前端测试用 `node --test`。
- `/v1/events` 不是持续事件流；没有 WebSocket 服务端。
- `laser-api` 直接依赖 `rusqlite`（规格要求禁止）。
- `laser-tools` 是单文件 crate，不是 16 个模块；`apps/mobile/` 不存在，Android 工程在 `apps/desktop/src-tauri/gen/android`。
- `moss-host` 只有 `serve` 与 `migrate` 两个子命令。
- 没有 `MOSS_RUNTIME_DIR` 环境变量，也没有 `.runtime/auth/host_key`；配对 token 不落库。
- 发送前不再要求 `$30=1000`，而是按设备实际 `$30` 在线换算 S 值。
- `laser_core::safety::evaluate_material`（PVC 阻断、塑料澄清）只有 `laser-core` 示例调用，`laser-api`、`laser-tools`、`laser-mcp` 都未接线；材料安全目前只由小智提示词与人工核对约束。
- Excalidraw 字体没有自托管到 `frontend/public/`，由 Vite 打进 `dist/assets`。
