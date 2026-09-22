# 开发者安全入口

改高风险代码前先读这页，再进入源码、测试和相关文档。这里是入口，不替代代码里的门禁。仓库级规则见 [`AGENTS.md`](../../AGENTS.md)。

## 1. 先确认运行时现实

- MCP 工具由 `laser_tools::ToolCatalog` 登记、`laser_mcp::dispatch_tool_call_with` 分发；小智连接由 `laser_host::HostApplication::spawn_mcp_bridge` 在 `MCP_ENDPOINT` 有效时启动。桌面端与 `moss-host serve` 走同一条路径。
- 目录共 20 个工具。`command_execution_tool` 只在 `MOSS_ENABLE_DEVELOPER_COMMANDS=true` 时进入列表并允许执行；`/v1/capabilities` 的 `command_execution` 字段反映该开关。
- 没有摄像头、Home Assistant、LaserGRBL GUI 自动化工具。文档、提示词或代码里不得把它们写成可用。
- `laser_grbl_tool` / `laser_network_grbl_tool` 只保留 `status` / `check` / `probe` 与 `cancel`；`send_file` 与 `send_command` 一律拒绝并引导走 workflow 或 `laser_safe_action_tool`。
- `ai_laser_gcode_tool`、`generate_text_image_gcode_tool`、`generate_text_laser_task_tool` 只接受 `text` 素材，都转发到 `/api/text-task/generate`；图片素材只能从网页或桌面端上传后走 workflow。
- 设备传输只有串口与 Telnet。没有 HTTP `/command`、`[ESP800]` 或 WebSocket 设备通道。

## 2. 高风险类别

以下改动默认按高风险处理，先读本页第 3 节，再读对应源码与测试：

- 完整发送链路：`laser-api` 的 `launch_send_job_for`、workflow `confirm_send`、材料矩阵 `send`、`/api/device/framing`。
- 设备层：`laser-device` 的握手、预检、`JobWorker` 流控、实时字节、S 值换算、SD 脱机上传与监控。
- 安全动作：`laser_tools::execute_laser_safe_action` 与 `/api/safe-action`，包括 `unlock`、`home`、`set_origin`、`laser_off`、`absolute_move`、`relative_move`、`emergency_stop`、`soft_reset` 与进给/功率倍率。
- 退出路径：`laser_host::ShutdownCoordinator`、panic 钩子、控制台关闭钩子、Tauri 关闭事件与托盘退出。
- 本地命令与桌面自动化：`command_execution_tool`、`open_app_tool`、`shortcut_key_execution_tool`（Windows 上通过 PowerShell `SendKeys`）。
- 局域网访问：`AuthManager`、`/v1/auth/*` 与公开路由白名单。

不要用 `command_execution_tool`、桌面自动化、HTTP 逐行发送或前端直连来绕过 workflow 的确认门和在线检查。

## 3. 完整发送的门禁

顺序与错误码以 `launch_send_job_for` 为准，任一失败都不写入加工指令：

1. HTTP 层要求 `confirmed=true`，否则 400；`laser-mcp` 在转发前同样检查。
2. `authorize_send`：记录处于 `pending`；重新读取物理 G-code 文件计算 SHA-256 并与冻结值比对；存在有效 workflow 快照或 calibration SHA。改材料、厚度、文件、策略、参数或连接目标后必须重新预览。
3. 同一设备已有在途作业：409 `device_busy`。
4. 打开传输失败：`device_unreachable`。
5. 有界握手（3 秒）：发 `$$` 并消费到终止 `ok`，要求 `$32=1`，读取 `$30`、`$10`；失败 `device_not_ready`。握手无响应时提示可能正在跑 SD 脱机作业。
6. `$CD` 读取 FluidNC 配置；旧 GRBL 静默跳过。
7. 自适应预检：`$30` 必须为有限正数，非 1000 时按 `$30/1000` 在发送流水线中换算 S 值，原文件与 SHA 门禁不变；不写 `$30` / `$32`。发 `?` 要求 Idle，拒绝 Run/Jog/Home/Hold/Door/Sleep/Check/Alarm、SD 运行标记及 `Pn:` 含 `E`/`F`/`D`；失败 `device_preflight_failed`。
8. 行校验：剥注释、丢空行；超过 254 字节或含非 ASCII 拒绝整个文件（`gcode_line_invalid`）；空文件拒绝（`gcode_empty`）。
9. 流式发送后保持作业占用，`G4 P0` 同步 ACK 与后续 Idle 都通过才标记 `completed`。断线、复位横幅（`Grbl `、`[MSG:INFO: Restarting]`、`[VER:`）、报警或无法确认状态都是"执行结果未知"，不自动重发。
10. `cancel` 只停止后续发送并走取消序列；`0x18` 走实时通道，不排队；写入成功不证明物理停机。`0x18` 不替代硬件急停。

其他固定规则：

- 生成、预览、推荐参数、反馈调参、走边框预览 G-code 都不等于启动机器。例外是 `/api/device/framing`：它真实驱动机器沿外接矩形运动（默认 `power=1`），改动时按发送链路对待。
- `laser_safe_action_tool` 中 `status` 与进给/功率倍率不需要确认；其余动作必须 `confirmed=true`（或 `dry_run=true` 只返回计划）。
- 网络完整任务只走 Telnet；SD 脱机模式的 HTTP 只用于把文件上传到 SD 卡。
- 不猜串口号、设备 host、文件名、材料、厚度、密码、token 或用户私有路径；`resolve_connection_spec` 只在参数缺失时回退到 `GRBL_DEFAULT_PORT` / `LASER_NETWORK_HOST`，且忽略含 `<` 的占位值。
- 局域网 token 只在内存；配对码只允许回环签发。

## 4. 已知未接线项

改相关代码时先核对这些现状，不要按文档或提示词想当然：

- `laser_core::safety::evaluate_material`（PVC 阻断、塑料澄清）没有被 `laser-api`、`laser-tools`、`laser-mcp` 调用，只有 `laser-core` 示例使用。材料安全目前只靠小智提示词与人工核对。
- 后端生成画板光栅时的工作区上限为 400×400 mm，不是翼宿的 100×100；行程限制以握手读到的 `machine_limits` 透传给前端约束画板。
- `jobs`、`auth_pairings`、`audit_log` 表已建但运行时无写入方；进度只在内存中的作业注册表与 workflow / calibration 记录的 `send_result` 里。

## 5. 测试要求

- 默认验证命令与构建要求见 `AGENTS.md`。
- 串口、HTTP、Telnet、子进程、GUI、文件系统副作用必须 mock；设备层用 `FakeGrbl` 做字节级校验，E2E 用回环 GRBL 模拟器。
- 连通性探测必须真实握手或在 `FakeGrbl` 中校验；刀路门禁必须对物理文件重算 SHA-256；急停 `0x18` 不得排在 G-code 队列之后。
- 未连接硬件、未实机出光、未跑通物理链路的结果标 `unverified` 或 `partial`，不得宣称 PASS。

## 6. 文档和提示词同步

- 改工具名、参数、注册开关、默认连接方式或确认规则时，同步 [`docs/integration/xiaozhi-prompts/latest.md`](../integration/xiaozhi-prompts/latest.md) 并新增日期快照；控制台正文 ≤2000、知识库 ≤200 字符（空格与换行计入）。
- 改路由、字段、错误码时同步 [Web API 契约](api-contracts.md)；改表结构时同步 [数据库结构](db-schema.md)；功能可见变化同步 [功能字典](feature-glossary.md)。
- 文档只写占位符和规则，不写真实 token、IP、串口号、账号密码或本机路径。
