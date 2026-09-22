# Changelog

本项目所有重要的代码变更、新特性以及问题修复均记录在此文件中。
版本记录格式规范参考 **Keep a Changelog** 与 **cc-switch 发布说明范式（深度因果闭环）**，详细规则参见 `CLAUDE.md`。

---

## [Unreleased]

### 🚀 新功能 (Features)

- **[FluidNC/Integration] 深度集成 FluidNC 现代控制与网络架构（$CD 配置提取、SD 脱机加工、红光走边框 Framing 与自适应标尺）**：
  - **YAML 硬件配置自动感知**：新增 `parse_fluidnc_yaml` 与 `DeviceConnection::fetch_fluidnc_config`，在设备握手探测时自动发送 `$CD` 并解析机床 X/Y 最大行程（`max_travel_mm`）、激光功率曲线（`speed_map`）以及气吹（`air_assist`）与安全门（`door`）外设引脚；当设备为传统 GRBL 时自动回退至从 `$$` 的 `$130/$131` 提取尺寸，`/api/connection/check` 与 `/api/device/probe` 自动透传 `machine_limits` 与 `features`，前端画板可动态约束加工有效范围杜绝撞机。
  - **SD 卡 / LittleFS 脱机离线加工**：在 `laser-device` 中新增 `wait_until_sd_completed`，持续追踪状态报告中的 `|SD:xx.xx%,/sd/job.gcode|` 真实执行进度；在 `laser-api` 与前端确认弹窗中增加 `offline_sd` 脱机模式支持，解算完成后直接发起 `$SD/Run` 本地脱机加工，彻底规避长达数小时的任务因电脑休眠、手机切后台或 Wi-Fi 抖动导致工件报废。
  - **红光轮廓预览走边框 (Framing)**：在 `laser-core/src/gcode.rs` 中新增 `generate_framing_gcode` 与 `generate_paths_framing_gcode`，根据图元外接矩形自动生成微弱光斑（或纯空走）循环走边框刀路；后端新增 `/api/device/framing` 接口并在前端 `DrawPage.jsx` 侧边栏增加【走边框 (Framing)】专属按钮，加工前一键直观校准材料摆放位置。
  - **功率标尺动态自适应**：引入 `verify_job_ready_adaptive` 与 `rescale_gcode_line_s`，遇到默认 `$30=255` 的 FluidNC 固件时不再强行拒绝，在保持原文件 SHA-256 门禁安全的前提下于底层通信流水线中自动进行 S 值等比例缩放（如 1000->255、500->128）。

- **[Frontend/CNC] 雕刻下发全链路升级为数控机床级中控台（JobWorker 逐行下发 + 发光实时进度条 + 三标签遥测抽屉）**：
  - **顶栏双指示胶囊解耦**：将原本单一模糊的 `OFFLINE` 拆分为独立的【⚡ 硬件: OFFLINE/ONLINE】（专指 GRBL 激光硬件控制器）与【🤖 小智: CONNECTED/OFFLINE】（专指小智 AI 语音/MCP 网关），彻底消除“右上角离线但底部在线”的认知冲突。
  - **串口助手级真实端口枚举**：移除串口下拉菜单中硬编码的“自动检测串口”与“COM1~COM6预设”，对齐串口助手仅列出系统真实识别到的可用串口列表，未检测到时明确提示；支持启动自动静默探测与下发前自动兜底握手。
  - **监视舱视野优化**：彻底删除加工预览中央多余的十字准星与中心靶心线条，保留纯净的雷达网格与激光工件轨迹。
  - **核心操作卡片**：主页右侧监控舱下方内嵌醒目的大号【0x18 软复位物理急停 (E-STOP)】红色制动按钮（独占整行，带高危警示光晕），以及展示刀路总行数、物理 SHA-256 指纹与预估耗时的【刀路摘要胶囊】；全界面统一保留左侧主控区唯一个【启动 JobWorker 逐行下发】按钮。
  - **发光实时进度条 (STREAMING PROGRESS)**：激光青色发光渐变光槽与呼吸光晕，实时透视 `lines_sent / total_lines` 行数与百分比。
  - **三标签遥测控制台抽屉 (ConsoleDrawer)**：
    - `>_ 通信终端与状态监控 (CONSOLE FEED)`：等宽黑客风黑底终端，支持 `[DISPATCH]`、`[STREAM]`、`[SUCCESS]`、`[GRBL]`、`[ESTOP]` 色彩标签高亮与自动吸底；预览生成时规范输出 `[PREVIEW]` 标签及解算刀路行数与 SHA-256 状态；
    - `小智 MCP 桥接状态`：通过 `/api/system/info` 与 `McpTelemetry` 全局单例实时捕获小智客户端身份（`xz-mcp-broker v0.0.1`）、握手协议（`2024-11-05`）与最新请求（`tools/list`），并在下方内嵌【小智 JSON-RPC 实时交互流 (RPC AUDIT LOG)】终端视窗，真实逐帧打印小智请求流水，拒绝任何静态数据伪装；
    - `G-CODE 实时行视图`：代码高亮亮度升级（亮白高对比度文字 `#f1f5f9`、金属蓝行号 `#818cf8` 与高能青色执行行高亮），下发时根据 `lines_sent` 自动平滑滚动并将当前执行行高亮变青色（带 `►` 激光光标），并在右侧提供【清空视图】按钮便于快速清理重置大文件代码。
  - **跨端与响应式适配**：对移动端竖屏（360~430px）提供自适应横向 Tab 滑动与百分比满宽，桌面端（`moss-desktop.exe`）与 Android 端（`app-arm64-debug.apk`）已全量重建。
- **[McpBridge] 小智 WebSocket 桥接落地**：`moss-host serve` 与桌面端启动时读取 `MCP_ENDPOINT`（`wss://…?token=…`），以 tungstenite + rustls(ring, webpki 根证书) 同步客户端连接小智控制台，逐帧处理 `initialize / ping / tools/list / tools/call` JSON-RPC，断线按 1s→60s 指数退避重连，日志中 token 脱敏；未配置或仍为模板占位符时静默不启动 (`crates/laser-mcp/src/lib.rs` `run_bridge`、`crates/laser-host/src/lib.rs` `spawn_mcp_bridge`)。
- **[McpTools] 工具调用接入 moss-host 真实后端**：`laser_workflow_tool`、`run_calibration_grid_tool`、`select_calibration_cell_tool`、`material_params_tool`、`generate_text_laser_task_tool`、`start_tuned_job_tool`、`regenerate_text_laser_task_tool` 通过 `HostBackend` 转发到进程内 `ApiService`，与网页/桌面端共享同一套 SHA 门禁、SQLite 存储与 JobWorker；`refine_laser_params_from_feedback_tool` 实现有界调参规则（太焦/太浅/切不透/毛边/断线）。
- **[MaterialLab] 标定矩阵全链路真实化**：`/api/material-lab/preview` 调 `laser_core::material::calibration_matrix` 按行列/功率/速度区间生成单元格与 G-code 并落盘、冻结 SHA、写入 `calibrations` 记录；`/api/material-lab/send` 复用 workflow 同款门禁与 JobWorker 真实逐行下发；`/api/material-lab/select-cell` 把选中单元格功率/速度/次数写回材料库（文件 + SQLite）；新增 `/api/material-lab/status` 供轮询。前端渲染真实单元格参数并轮询发送进度。
- **[TypeBoundary] 类型边界守卫改为 Rust 集成测试** `crates/laser-contracts/tests/type_boundary.rs`，随 `cargo test --workspace` 执行，CI 不再依赖 Python。

### 🔄 变更 (Changes)

- **[Docs/Architecture] 全面重构文档体系与契约对齐**：
  - **结构规范化与分层归档**：将 `docs/` 根目录下分散的技术文档、测试报告与图片进行系统性分层，重组为用户（`docs/user/`）、开发者（`docs/developer/`）、激光硬件（`docs/laser/`）、小智集成（`docs/integration/`）与历史归档（`docs/archive/`）五大专属目录，全仓 Markdown 相对链接已全部通过自动化校验，杜绝任何断链。
  - **活文档重写与规范固化**：新增 `docs/developer/architecture.md`（真实反映 Rust 进程模型、crate 拓扑、同步 TCP 宿主与自适应流控）与 `docs/developer/frontend-behavior.md`（固化前端状态词表、重发防呆、覆层映射与本地存储键）；重写 `docs/user/quick-start.md`（整合原 Windows 指南，覆盖双模启动、配对与脱机操作）、`docs/developer/standards.md`、`docs/developer/safety.md`、`docs/laser/README.md` 与 `docs/laser/xiaozhi-grbl-tool-boundary.md`；更新 `api-contracts.md`（收录全部 28 个路由并修正字段错误）与 `db-schema.md`（对齐 SQLite schema v2 并标注未接线数据表）；更新小智提示词 `latest.md` 与历史快照。
  - **历史留痕与废弃清理**：新建 `docs/archive/` 并创建归档总览 `README.md`，将重构阶段性产物（`rust-rebuild-spec.md`、`MIGRATION_STATUS.md`、`ACCEPTANCE_REPORT.md`、`APP_BUTTONS_TEST_REPORT.md`、`frontend-inventory.md`、`voice-image-gcode-engine.md`）移入归档并统一追加只读归档横幅，移除硬编码的本地私有路径；彻底删除已废弃的旧版 Python `windows.md`、空重定向存根以及两份一次性展示 HTML；将 3 张非文档测试位图移入 `tests/fixtures/images/`。
- **[Frontend/Serial] 扩充串口波特率预设梯队**：在 `frontend/src/pages/HomePage.jsx` 中将原本单一的 3 个选项扩充为覆盖主流与高速场景的完整标准梯队（`9600`、`19200`、`38400`、`57600`、`115200 (默认)`、`230400`、`250000`、`460800`、`921600`），无缝适配 ESP32、FluidNC 极速流式传输与各类微控制器调试，保持 115200 为默认选中项。
- 整理公开仓库目录，区分应用、文档、脚本和资源。
- 补充贡献指南、安全策略和 CI 配置。
- **[Repo] 彻底移除 Python 运行时遗留**：删除 `pyproject.toml`、`venv/`、`migration/`（黄金样本生成与差分脚本）、`tests/test_*.py`（28 个用例文件均 import 已删除的 `core/tools/moss_mcp`，仅靠本机 venv 指向旁路目录才能运行）；`AGENTS.md`、`CONTRIBUTING.md`、`CLAUDE.md`、`docs/developer/*` 的验证命令统一为 `cargo test --workspace` + `npm test` + `npm run test:e2e`。黄金样本 `tests/golden/` 与字体夹具保留。
- **[McpTools] 兜底分支不再伪装成功**：未实现或需要后端的工具在无后端时返回 `success:false` 并说明原因；`laser_grbl_tool/laser_network_grbl_tool` 的 `send_file/send_command` 明确拒绝并引导走安全 workflow。
- **[Capabilities] `/v1/capabilities`** 的 `serial` 改为 true（串口传输已实现），`command_execution` 反映 `MOSS_ENABLE_DEVELOPER_COMMANDS` 开关。
- **[laser-mcp] 移除未使用的 `rmcp` 依赖**（仅存在一个编译探针），改为 `tungstenite 0.26` + `rustls 0.23(ring)`；依赖树不含 OpenSSL / aws-lc / tokio。

### 🐛 修复 (Bug Fixes)

#### [Testing/Contracts] 修复测试套件中的伪成功断言与 API 快照覆盖缺口
- **问题**：部分 E2E 用例使用本地变量、自造 ID、条件跳过和宽松终态判断；Rust 测试存在恒真断言、实际执行 shell 的测试替身、只校验 HTTP 状态码的 golden 测试；项目根目录缺少 workspace 清单，导致约定的 `cargo test --workspace` 无法执行。
- **修复**：移除伪造回退值并强制校验真实上传文件、工作流 ID、取消可恢复状态和 GRBL 通信证据；将命令执行测试改为注入 runner；补充根 workspace；golden 测试改为深度比较响应内容，并保持材料库导出协议对默认 `power_scale` 的兼容。
- **验证**：补充 E2E 临时目录清理和关键通信事件检查；通过 `cargo metadata`、相关 Rust crate 测试、前端单测和 `moss-host --release` 构建验证。

#### [Frontend/E2E-Tests] 彻底清理 E2E 测试套件中虚假凑数的自测自断言代码，剔除伪造闭环并排除测试临时脏目录
- **现象与影响 (Symptom & Impact)**：
  - 在此前 `frontend/tests/e2e/buttons.test.mjs` 中，宣称覆盖“37 项全功能 UI 按钮与状态闭环”，但在 `[BTN-11]`（日志清空）、`[BTN-21]`（路由返回）、`[BTN-22/23]`（场景与图元导出）、`[BTN-25]`（材质切换）、`[BTN-27]`（画板配置保存）、`[BTN-29]`（防呆模拟）、`[BTN-32]`（Tab 切换）、`[BTN-34]`（关闭策略）、`[BTN-36]`（Token 清除）、`[BTN-37]`（退出急停）等用例中，直接在测试闭包内部通过 `let` 局部变量自赋值、`new Map()` 原生集合存取或硬编码布尔值来强行满足 `assert.equal`；
  - 该行为并未引入或调用任何真实被测业务模块与 DOM 事件，虚构了 100% 通过的“伪绿灯”战报，严重掩盖了前端状态机与安全门禁的真实可用性，违背了测试真实性底线；同时运行 E2E 测试时在工作区残留了 `.tmp-test-e2e` 脏数据。
- **根因分析 (Root Cause Analysis)**：
  - 测试编写者为了在进度指标上达成“全量 UI 按钮自动化覆盖”，在尚未接入真实无头浏览器（Headless Browser / Playwright）自动化渲染与交互驱动的情况下，将原本需要驱动真实 UI 渲染的测试用例退化为测试文件内部的纯 JavaScript 变量赋值和原生 API 调用，形成了“自写变量、自调赋值、自言成功”的形式主义闭环。
- **修复措施 (Fix & Implementation)**：
  - ① 坚决果断彻底剔除 `frontend/tests/e2e/buttons.test.mjs` 中的 11 处虚假自测用例（`BTN-11, 21, 22, 23, 25, 27, 29(虚假防呆段), 32, 34, 36, 37`），剥离虚构的 UI 按钮包装；
  - ② 将该测试套件正名为“核心 API 与硬件通信工作流端到端真实测试”，仅保留经过真实 TCP 网络通信、回环 GRBL 仿真器握手、`$$` 固件校验、JobWorker 逐行 ACK、设备只读探测与后端 API 真实交互的 26 项高置信度硬核端到端用例；
  - ③ 彻底删除工作区下散落的 `.tmp-test-e2e` 与 `.tmp-test` 临时目录，并在根目录创建标准 `.gitignore` 规则确保测试脏产物绝不入库；
  - ④ 确立“先核对真实需求再写测试”的原则，真正的前端 UI 逻辑测试严格通过专有的前端单元测试（如已有的 40 项单测）或后续经核对授权的真实端到端驱动实现。
- **验证证据与回归测试 (Verification & Regression)**：
  - 执行 `cd frontend && npm test`，确认已有的 40 项前端核心单元测试（覆盖 `repeatSendGuard`、`appDialog`、`authSession`、`materialSelectState`、`statusNormalize` 等）全量通过且均引入了真实源文件；
  - 确认 `frontend/tests/e2e/buttons.test.mjs` 与 `scripts/test_app_buttons.mjs` 中虚假自测用例完全清零，测试命名如实反映实际覆盖范围。
- **关联代码与索引 (References)**：
  - `frontend/tests/e2e/buttons.test.mjs`；
  - `scripts/test_app_buttons.mjs:184-188`；
  - `.gitignore`；
  - `memory/honest-meaningful-testing.md`。

#### [LaserDevice/FluidNC] 字符计数流控升级为通道感知与动态自适应，消除 127 字节瓶颈与雕刻微停顿焦斑
- **现象与影响**：在雕刻复杂矢量曲线或高密度微小线段图案时，ESP32/FluidNC 激光雕刻机在拐角处仍偶发微停顿，在加工材料上留下焦黑凹斑，且整体加工速度受限于上位机单步发包节奏。
- **根因分析**：`JobWorker` 固定采用了旧版 8 位 AVR 单片机的 `DEFAULT_RX_BUFFER_SIZE = 127` 字节配额；而现代 FluidNC 运行在 ESP32 上，串口/网络接收缓冲实际为 512~1024 字节，规划器缓冲多达 35 块；固定 127 字节在面对微线段时无法充分填满下位机规划器，导致规划器饥饿（Planner Starvation）。
- **修复措施**：① 传输层抽象增加 `default_rx_buffer_size()`，网络连接（`TcpGrbl`）初始默认窗口提升至 1024 字节，物理串口（`SerialGrbl`）初始保持 255 字节安全向下兼容；② 在 `JobWorker::stream_lines_with_buffer_size` 的响应消费循环中，动态解析状态报告中的 `|Bf:blocks,rx_bytes`，将流控窗口自适应扩容至硬件真实报告的空闲容量（`clamp(255, 1024)`），大幅提升预推密度并消除微停顿。
- **关联代码**：涉及 `crates/laser-device/src/lib.rs:110, 195, 230, 1140-1230`，配套单测 `stream_lines_dynamically_adapts_to_reported_rx_buffer`。

#### [LaserDevice/Diagnostics] 报错与报警诊断全量扩充覆盖 FluidNC 专有 Alarm 11~18 与 Error 码体系
- **现象与影响**：当机床发生启动引脚冲突、未回零锁定或 SD 卡未插入等故障时，上位机控制台与小智语音仅能输出抽象数字代码或未知错误（如 `ALARM:11`、`error:60`、`error:150`），用户无法获知真实的物理故障原因，排查成本高。
- **根因分析**：旧实现中 `diagnose_device_message` 仅内置了老版 GRBL 基础报警（1~10 号）与 7 个常见 G-code 语法错误码，缺少 FluidNC 现代体系针对外设引脚、脱机文件系统和安全策略的专有扩展映射。
- **修复措施**：在 `diagnose_device_message` 中全量扩充对 FluidNC 专有代码的自然语言翻译：① 报警体系新增 11（启动引脚触发）、12（回零歧义）、13（硬停止）、14（强制回零锁定）、15（配置加载锁定）、16（IO扩展复位）、17（几何冲突）、18（探针撞限位）；② 错误体系新增 40（启动引脚异常）、60（SD卡挂载失败/未插）、61/66（文件打不开）、65（文件不存在）、67（文件系统忙）、80/81（数值无效）、110（Web认证失败）、120（接口被WebUI占用）、130（点动取消）、150/152（引脚配置或YAML语法错误）。
- **关联代码**：涉及 `crates/laser-device/src/lib.rs:1290-1360`，配套单测 `diagnose_device_message_translates_fluidnc_alarms_and_errors`。

#### [LaserApi/OfflineSD] SD 卡脱机作业实现任务唯一指纹命名、HTTP 上传与硬件 SHA-256 物理对账闭环
- **现象与影响**：脱机雕刻任务原先仅向机床发送 `$SD/Run=/sd/job.gcode` 指令，未在下发前执行文件上传与校验；若 SD 卡中无此文件或存在上一任加工的旧文件，机床直接报 `error:65` 或误雕刻上一次的历史任务，造成材料报废与出光安全风险。
- **根因分析**：缺少面向机床存储介质的传输与对账闭环；使用固定的文件名称极易引发同名文件覆写残留与状态混淆。
- **修复措施**：① 引入任务与哈希唯一命名规则 `/sd/moss_{safe_wf_id}_{short_sha}.gcode` 杜绝命名冲突与残留覆盖；② 支持通过机床 Web/HTTP API 自动将解算好的物理 G-code 上传至 SD 卡；③ 上传后自动发送 `$File/ShowHash` 请求机床固件自计算并核验 SHA-256 物理指纹，哈希不匹配即刻阻断；④ 握手校验一致后发送 `$SD/Run`，并通过 `wait_until_sd_completed` 实时捕获固件状态报告中的 `|SD:xx.xx%|` 真实加工进度直至完成。
- **关联代码**：`crates/laser-api/src/lib.rs:600-665`、`crates/laser-device/src/lib.rs:1320-1420`。
