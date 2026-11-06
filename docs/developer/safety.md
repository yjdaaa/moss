# 开发者安全入口

改高风险代码前先读这页，再进入源码、测试和相关文档。这里是入口，不替代代码里的门禁。仓库级规则见 [`AGENTS.md`](../../AGENTS.md)。

## 1. 先确认运行时现实

- MCP 工具由 `laser_tools::ToolCatalog` 登记、`laser_mcp::dispatch_tool_call_with` 分发；小智连接由 `laser_host::HostApplication::spawn_mcp_bridge` 在 `MCP_ENDPOINT` 有效时启动。桌面端与 `moss-host serve` 走同一条路径。
- 目录共 20 个工具。`command_execution_tool` 只在 `MOSS_ENABLE_DEVELOPER_COMMANDS=true` 时进入列表并允许执行；`/v1/capabilities` 的 `command_execution` 字段反映该开关。
- 激光工具目录里没有摄像头、Home Assistant、LaserGRBL GUI 自动化工具，提示词不得把它们写成激光工具。内置语音另有视觉（NV12）和 Home Assistant（NV15）工具；HA 控制默认关闭，由用户在私密窗口同意后开启、每个动作逐次桌面批准，见 §4.17。
- `laser_grbl_tool` / `laser_network_grbl_tool` 只保留 `status` / `check` / `probe` 与 `cancel`；`send_file` 与 `send_command` 一律拒绝并引导走 workflow 或 `laser_safe_action_tool`。
- `ai_laser_gcode_tool`、`generate_text_image_gcode_tool`、`generate_text_laser_task_tool` 只接受 `text` 素材，都转发到 `/api/text-task/generate`；图片素材只能从网页或桌面端上传后走 workflow。
- 设备传输只有串口与 Telnet。没有 HTTP `/command`、`[ESP800]` 或 WebSocket 设备通道。

## 2. 高风险类别

以下改动默认按高风险处理，先读本页第 3 节，再读对应源码与测试：

- 完整发送链路：`laser-api` 的 `launch_send_job_for`、workflow `confirm_send`、材料矩阵 `send`、`/api/device/framing`。
- 设备层：`laser-device` 的握手、预检、`JobWorker` 流控、实时字节、S 值换算、SD 脱机上传与监控。
- 安全动作：`laser_tools::execute_laser_safe_action` 与 `/api/safe-action`，包括 `unlock`、`home`、`set_origin`、`laser_off`、`absolute_move`、`relative_move`、`emergency_stop`、`soft_reset` 与进给/功率倍率。
- 退出路径：`laser_host::ShutdownCoordinator`、panic 钩子、控制台关闭钩子、Tauri 关闭事件（含 macOS `RunEvent::Exit`）与托盘/菜单栏退出。
- 本地命令与桌面自动化：`command_execution_tool`、`open_app_tool`（Windows 路径/URI，macOS `/Applications/*.app` + `open -a`）、`shortcut_key_execution_tool`（Windows 上通过 PowerShell `SendKeys`，macOS 上媒体/音量白名单走 `osascript`）。
- 局域网访问：`AuthManager`、`/v1/auth/*` 与公开路由白名单。
- 浏览器来源校验（CORS）：`laser-api` 的 `is_allowed_origin`、`handle_http_from`、`MOSS_ALLOWED_ORIGINS` 解析（`laser_runtime::config::parse_allowed_origins`）。不得恢复 `Access-Control-Allow-Origin: *`，不得把来源校验改成前缀匹配；回环连接的 `Host` 白名单（`is_allowed_loopback_host`，防 DNS 重绑定）不得放宽成任意主机名；回环请求免鉴权，所以跨站网页的 POST 必须在路由前以 403 `origin_not_allowed` 拦下（见 [api-contracts.md](api-contracts.md) §1.6）。

不要用 `command_execution_tool`、桌面自动化、HTTP 逐行发送或前端直连来绕过 workflow 的确认门和在线检查。

## 3. 完整发送的门禁

顺序与错误码以 `launch_send_job_for` 为准，任一失败都不写入加工指令：

0. **严格模式授权**（`MOSS_FUSION_STRICT=true` 时，技术书 §6.2；默认关闭，关闭时本步不读取任何数据、不改变判定）：**开关在语音中心顶部（n27，n28 修订，2026-10-18）**——点开需二次确认；开启与关闭都立即生效，并把值写入运行目录 `.env` 的 `MOSS_FUSION_STRICT`（保留其他行与注释，原子写）。关闭会同时更新启动快照与运行时值（`set_strict_from_env` + `set_strict_mode`），所以不存在“要重启才生效”的中间态；**自建链路仍强制开启，命令与界面双重拒绝关闭**（`strict_mode_locked`），切换到自建链路时 `moss-voice` 会重新强制开启。唯一检查点 `require_r3_authority`（`crates/laser-api/src/approval.rs`）。桌面会话放行；小智桥接 / 局域网配对 / 网关只创建审批并返回 HTTP 202 `approval_required`，不连接设备；本机回环无凭据返回 403 `strict_mode_blocked`；以审批身份执行时（不论严格模式开关）按当前要素与审批保存的连接重算摘要并单次消费，执行使用审批保存的连接参数。无本地数据库时 503 `approval_unavailable`（失败即拒绝）。同一检查点也接在 `/api/safe-action` 的 R3 动作（`laser_contracts::safe_action_requires_approval`：除查询、急停、软复位、关激光、倍率复位与降低外一律视为 R3）与 `/api/device/framing`。停止类动作永不追加条件。
1. HTTP 层要求 `confirmed=true`，否则 400；`laser-mcp` 在转发前同样检查。
2. `authorize_send`：记录处于 `pending`；重新读取物理 G-code 文件计算 SHA-256 并与冻结值比对；存在有效 workflow 快照或 calibration SHA。改材料、厚度、文件、策略、参数或连接目标后必须重新预览。
3. 同一设备已有在途作业：409 `device_busy`。
4. 打开传输失败：`device_unreachable`。
5. 有界握手（3 秒）：发 `$$` 并消费到终止 `ok`，要求 `$32=1`，读取 `$30`、`$10`；失败 `device_not_ready`。握手无响应时提示可能正在跑 SD 脱机作业。
6. `$CD` 读取 FluidNC 配置；旧 GRBL 静默跳过。
7. 自适应预检：`$30` 必须为有限正数，非 1000 时按 `$30/1000` 在发送流水线中换算 S 值，原文件与 SHA 门禁不变；不写 `$30` / `$32`。发 `?` 要求 Idle，拒绝 Run/Jog/Home/Hold/Door/Sleep/Check/Alarm、SD 运行标记及 `Pn:` 含 `E`/`F`/`D`；失败 `device_preflight_failed`。
8. 设备预检后再读取物理 G-code，与确认快照的 SHA-256 重新比对；不一致返回 `send_gate_rejected`，不写入加工行。**任务范围复核**（2026-11-06）：按这份内容计算激光头走过的 X/Y 范围（`laser_core::gcode::motion_extents`，跟踪 G90/G91、G20/G21、`G92` 改原点与 G2/G3 圆弧，快速移动也计入），宽或高超过握手 `$130/$131` 或 `$CD` `max_travel_mm` 报告的行程时返回 400 `job_exceeds_travel`，不写入加工行；固件未报告行程或文件含 `G53` / `G28` / `G30` / `G10` 等无法从文件得知终点的指令时跳过本项、交给固件软限位。任务以开始时激光头位置为原点，因此只比较宽高、不判断起点位置。`/api/device/framing` 同样复核。之后的流式发送和 SD 上传只使用这份已比对的内存内容。行校验剥注释、丢空行；超过 254 字节或含非 ASCII 拒绝整个文件（`gcode_line_invalid`）；空文件拒绝（`gcode_empty`）。
9. 流式发送后保持作业占用，`G4 P0` 同步 ACK 与后续 Idle 都通过才标记 `completed`。断线、复位横幅（`Grbl `、`[MSG:INFO: Restarting]`、`[VER:`）、报警或无法确认状态都是"执行结果未知"，不自动重发。
10. `cancel` 只停止后续发送并走取消序列；`0x18` 走实时通道，不排队；写入成功不证明物理停机。`0x18` 不替代硬件急停。

其他固定规则：

- 生成、预览、推荐参数、反馈调参、走边框预览 G-code 都不等于启动机器。例外是 `/api/device/framing`：它真实驱动机器沿外接矩形运动，改动时按发送链路对待。2026-10-14 起默认 `power=0` 不出光（弱光需显式传 `power`），与完整发送相同做同设备占用检查、握手与 Idle 预检，运行期间登记为活动作业 `framing-…`，顶栏急停与带 `job_id` 的 `emergency_stop` 可停止；失败或被停止后保留为“待确认停止”目标。
- `laser_safe_action_tool` 中 `status` 与进给/功率倍率不需要确认；其余动作必须 `confirmed=true`（或 `dry_run=true` 只返回计划）。
- 网络完整任务只走 Telnet；SD 脱机模式的 HTTP 只用于把文件上传到 SD 卡。
- SD 上传后核对固件返回的文件哈希；固件不返回哈希（未启用 HashFS）时**拒绝 `$SD/Run`**，任务记为 failed（2026-10-14 用户决定，此前为继续执行并警告）。软件比对通过也不能当作实机验收。
- 不猜串口号、设备 host、文件名、材料、厚度、密码、token 或用户私有路径；`resolve_connection_spec` 只在参数缺失时回退到 `GRBL_DEFAULT_PORT` / `LASER_NETWORK_HOST`，且忽略含 `<` 的占位值。
- 局域网 token 只在内存；配对码只允许回环签发。

## 4. 当前边界与已知未接线项

- NV9 第三方 MCP 已接入桌面内置语音：配置命令只能来自主窗口当前实际回环来源，不提供 HTTP 管理接口；stdio 仍具有当前用户系统权限，不是沙箱。工具默认 Ask，不因激光严格模式关闭或第三方声明只读而免批。名称、64 工具预算、停用状态在列表和调用处校验；来源会话、凭据／配置／schema 与参数绑定单次审批，改配置或撤销设备使未消费审批失效。执行前审计失败不放行；执行后写失败、超时或掉线不自动重发。详见 [第三方 MCP](../integration/moss-external-mcp.md)。

改相关代码时先核对这些现状，不要按文档或提示词想当然：

- `laser-api` 的文字、图片、G-code workflow 预览，以及 workflow / 标定矩阵发送和断点恢复，都会调用 `laser_core::safety::evaluate_material`。PVC 与明确危险材料直接返回 `material_safety_blocked`；仅写“塑料”等成分不明描述时要求澄清。小智提示词仍负责对话层确认与实物复核，后端也会独立拒绝，不依赖提示词是否遵守。
- 画板、图片与文字预览都按机器幅面（默认 100 × 100 mm，请求可覆盖）限制尺寸（2026-11-06 起画板不再使用 400 mm 画布）；发送前另按固件报告的行程复核刀路范围（`job_exceeds_travel`）。生成的文字任务默认以激光头所在位置为木牌（图案）中心（`G92 X<半宽> Y<半高>`），图片任务以激光头位置为图片左下角。
- 经审批执行的发送在开始前把审批结果记为“发送中”（`phase=running`），发送线程结束时写回完成 / 失败 / 取消；进程在发送中崩溃后，启动恢复把这类审批记为失败、`error_code=result_unknown`，workflow 同时被记为 failed，任何一方都不会自动重发（A-33）。
- 内置语音（NV4）以 `CallerContext::Voice` 调用工具。运动与出光（R3）**跟随 `MOSS_FUSION_STRICT`**（2026-10-07 用户决定）：开启时只生成审批（来源 `voice`），关闭时与官方接入点相同，模型带 `confirmed=true` 即执行——此时唯一的确认是系统提示要求模型先复述参数、等用户口头确认，没有桌面批准。2026-11-06 起（技术书 r18）AI（语音、小智接入点、网关）生成的每个预览都会在桌面弹出预览窗（`AiPreviewCenter`），用户可在窗内勾选现场确认后“同意开刻”或取消；严格模式开启时只能在界面同意，关闭时口头确认后 AI 仍可 `confirm_send`。文字生成工具的 `confirmed + send_after_generate` 不再顺带发送，只生成预览。R4 与未分级工具（命令执行、打开应用、快捷键、打开网站）**不论开关都拒绝**（`HostBackend::always_deny_r4`），返回 `tool_not_allowed` 并记审计 `voice:<设备>`；工具清单与严格模式下的接入点相同（`laser_host::voice_tools_list`）。停止类动作照常立即执行。
- 工具开关（NV8，`mcp_disabled_tools`）只能减少可用工具：关闭的工具在接入点与内置语音的清单和调用两处都拒绝（`tool_disabled`，不触达后端）。`laser_safe_action_tool`（停止、急停）列在 `ALWAYS_ENABLED_TOOLS`，后端与语音中心都不允许关闭；新增停止类工具时须同时加入该列表。
- 小智接入点的工具分级在 `laser_contracts::tool_risk`（R0 只读、R1 生成、R2 降低风险、R3 运动或出光、R4 永不开放）。严格模式下接入点对 R4 与未分级工具返回 `tool_not_allowed`，R3 交给执行层生成审批；新增 MCP 工具时必须同时补分级，否则 `laser-mcp` 测试失败。
- 严格模式下桌面端通过 `get_desktop_session` 携带 `X-Moss-Desktop-Session` 放行；不带桌面会话头的普通本机回环请求返回 403 `strict_mode_blocked`。
- `audit_log` 自 2026-10-06（P1-08）起由审批服务与接入点写入：审批创建 / 批准 / 拒绝 / 过期 / 执行 / 失败、模型自填 `confirmed` 被删除、R4 拒绝、严格模式拦截。创建、批准、执行三步与审计在同一事务中，审计写不进去就拒绝（R3 不放行，状态不变）；拒绝、过期、失败与停止类动作不依赖审计。
- `jobs`、`auth_pairings` 表已建但运行时无写入方；进度只在内存中的作业注册表与 workflow / calibration 记录的 `send_result` 里。
- S1-09 Android 实际来源（`http://tauri.localhost`）必须经安装设备核验，不得用 APK 构建通过冒充实机来源验收。

### 4.1 语音授权与停止修补（2026-10-10）

设备 token 校验后还需注册当前 device/client/session。撤销使所有旧会话标记永久失效，会话循环检查后关闭连接；重新允许只能建立新会话。工具执行入口再次检查，尚未消费的该设备审批同事务过期，预检或排队期间失效的语音发送不启动加工。已经执行的指令不因撤销自动撤回，桌面停止能力始终保留。

停止快捷通道只接受最终识别的完整短句，不因中间词“停止”抢先执行；仍在大模型前处理。无活动任务时需已明确配置的目标与现有 confirmed 契约，多任务未指定目标不随机选择；状态查询失败不等于任务失败，也不等于停机。界面持续重试并保留急停，请求写入成功只显示“已发送，停止未确认”。

本地判停在独立工作线程运行，事件循环不等待 IPC；队列和并发线程有界，排队到响应超时关闭异常会话，旧录音轮次的结果丢弃。不能宣称被阻塞的模型计算一定已被强制终止，也不能以软件通过代替实体急停或实机性能验收。

### 4.2～4.8 NV13 沙箱边界与离线测试夹具约束（生产规则见 §4.18）

- **进程树隔离边界（Windows Job Object / Unix 独立进程组）**：原版 `codex-cli` 的普通只读模式无法阻止读取工作区外文件或连接本地回环端口；Windows Job Object（`windows_process.rs`）与 Unix/macOS 独立进程组（`unix_process.rs`：`process_group(0)` / `setpgid(0, 0)` + 下发任务 `stdin` 前二次授权复核 + `kill(-pgid, SIGKILL)` + `ESRCH` 回收）仅约束进程树生命周期、时长与输出上限，**不限制用户文件读取、网络或 CLI 自身登录账号**。禁止为绕过限制而自动启用提权沙箱或修改 OS ACL。
- **Docker 与 `model_proxy` 离线夹具（未接入生产）**：`scripts/test-nv13-container-isolation.ps1`、`voice_agent/docker_plan.rs` 与 `laser-host::voice_agent::model_proxy`（`transport`、`egress`、`tls_client`）仅作离线安全测试夹具，不暴露生产端口、不接入生产 Agent 执行。测试容器必须保持 `--rm --pull never --network none --read-only --cap-drop ALL --security-opt no-new-privileges --pids-limit 8`、非 root 与唯一只读合成工作区挂载；严禁加 `--privileged`、`CAP_SYS_ADMIN`、`seccomp=unconfined`，严禁向容器挂载 MOSS 目录、Windows 用户主目录、Docker socket 或真实长期密钥。详见 [`docs/integration/nv13-codex-docker-security.md`](../integration/nv13-codex-docker-security.md)。

### 4.9 NV14 本地音乐选择、播放和撤销隔离（2026-10-11，软件完成 / 设备待验收）

音乐默认关闭且无目录，不自动扫描任何个人文件。只在用户显式启用并保存本地目录后，由桌面主窗口主进程验证 8 层、10,000 曲目/5,000 目录/50,000 项限额和本地安全根，跳过敏感子目录（含 Windows `AppData`、macOS `Library`/`.Trash`/`.ssh` 等），拒绝 UNC、链接/Windows reparse、敏感系统根（含 Windows 系统卷根与 macOS `/System`、`/Library`、`~/Library`、`/Applications`、`/Volumes` 顶层挂载根；macOS 仅豁免系统卷根 firmlink `/var`、`/tmp`，任何用户软链组件一律拒绝）；受信索引生成 `track_id`，重名候选仅显示非路径索引 ID 与编号（不明确选择就不播放），模型不能提供任意路径/URL 或用音乐混合其他工具。读取前按打开文件实际句柄（Windows `GetFinalPathNameByHandleW`、macOS `O_NOFOLLOW` + `fcntl(F_GETPATH)`、Linux `/proc/self/fd`）、size/mtime 重新检查。音频经 Symphonia 逐包转 24 kHz PCM，队列≤20×100ms、会话泵≤1秒；打断/新轮次/断线/撤销使用 generation、取消及发送前许可复核，歌曲不进入 LLM 历史。`voice_music_status`/`save`/`pick_directory` 只给桌面 `main` 窗口，选择文件夹只在显式点击后弹框且不自动扫描。

### 4.10～4.15 NV15 家居控制、备份迁移与日志脱敏安全门禁（生产开启规则见 §4.17）

- **schema 校验与状态裁剪**：`laser-host::voice_home` 校验 HA 状态与服务 schema；未经用户逐实体选择、当前服务不存在、状态 `unknown`/`unavailable`、模型输入篡改增加属性、危险高风险域自动放行策略一律拒绝。对外可用状态仅保留最少的温度/亮度/单位数字，任意 PIN、密码、token 及嵌套属性全部丢弃。
- **网络出口与单次提交闭环**：只读 GET 与控制 POST 共享 `moss_voice::home_readonly::pinned_client`（DNS/IP 私网检查、一次性固定解析、HTTPS 校验、无代理、不跟随重定向）。HomeControl 将 endpoint、凭据 revision、实体策略 revision、设备许可、动作、目标、参数摘要绑定到专用持久事务审批，要求审计成功才可操作。每个审批消费后**至多一次 POST**；敏感 token/config 改动、取消与唯一 POST 发送边界共享同一 `send_fence` 并用 `BEGIN IMMEDIATE` CAS 串行化，最终状态与审计同事务入库。收到与否不明确、HTTP 错误、提交后撤销，或目标状态回读（最多 3 次只读 GET，总预算 3 秒、单次上限 900ms）期间设备许可被撤销，**即使旧 GET 返回匹配状态也不得写入 `Completed`**，一律持久记为 `result_unknown` 且绝不自动重发。
- **可移植备份与跨账户系统凭据封存保护**：Windows 用当前用户 DPAPI（`dpapi-user-v1:`），macOS 用当前登录钥匙串随机主密钥（`Security.framework`，`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`）派生的 Encrypt-then-HMAC-SHA256 信封（`keychain-user-v1:`）。仅允许从 `voice-private` 窗口导出/导入固定受限 `moss-home-profile-v1` 白名单（仅含不带 origin、媒体库 ID、许可或已启用标志的实体清单），禁止打包 SQLite/WAL/SHM 或迁移原始 `dpapi-user-v1:` / `keychain-user-v1:` 密文（`home_backup_audit` 对两类前缀均标记为不可分享的加密凭据材料）。恢复须二次确认，先原子撤销本机旧 HA token、voice read/control grants 和待审批 HomeControl 并将所有实体置为禁用，重新输入令牌并再次审批后才可运行。遇到其他系统或账户的密文时仅允许受限启动以执行确认撤销迁移（单调版本号原子清除旧密文），任何读取凭据、HA 请求或控制批准均拒绝。已发送的外部控制无法撤回；疑似泄漏须去 HA 服务端轮换/撤销 token。
- **NV15-1 历史备份审计与日志脱敏**：用户在 `voice-private` 页面点击按钮后由 Rust 原生文件选择器选择单个本地普通非链接文件（拒绝前端传路径、`..`、reparse 组件、网络路径及 >64MiB），只读分块检查标记，仅输出固定类别/字节数/禁止分享标志，不输出路径或原文。`moss_voice::redact::redact_line` 与 `RotatingLog::tail` 对常见凭据字段和 Bearer/Basic 头做重脱敏，但不替代完整 DLP；不得将旧日志、SQLite/WAL、原始诊断转储或备份判定为可安全分享。

### 4.12 NV18 语音 sidecar 构建资源安全（2026-10-11）

`build-windows-installer.ps1` 入口已转到独立 `build-nv18-sidecar-safe.ps1`；按次只生成新 GUID 资源目录，禁止递归删除或覆盖用户已有 `bundle-resources/voice-local`。对应 helper/源码/COPYING/初步 notices 已构建进入 NSIS；`verify-nv18-nsis-in-docker.ps1` 以禁网只读非 root Docker 列出真实安装包并验证上述条目，清理自身检查镜像。**还未满足 sherpa-onnx/espeak-ng 及静态依赖的完整许可证/对应源码审计；不得因此对外分发**。安装/升级/卸载、数据迁移和真实 helper 启动均需明确授权的安全环境验收；构建过程不得运行安装器或连接真实硬件。

### 4.13 语音中心插件/私人记忆的 Tauri ACL 本地页面隔离（2026-10-11，2026-10-18 n26 更新）

桌面主窗口继续导航到本机 `http://127.0.0.1:<端口>/`，该页面对 Tauri 来说属于 remote origin。n26（2026-10-18 用户决定）起，插件密钥状态与私人记忆可在主语音页就地展开：授权写在独立的 `capabilities/voice-private-main.json`（`windows:["main"]` + `remote:["http://127.0.0.1:*"]`），命令开头用 `require_private_settings_window` 校验「打包私密页 URL」或「经 `window_is_trusted_main` 确认的主窗口精确来源」，其他 host、端口、路径、query 一律拒绝。`voice-private` 窗口仍只加载安装包资源 `voice-private.html`，其四个 capability 不放宽也不配 `remote`。**固件导入与推送 `voice_firmware_{status,import,manage}` 仍只认 `voice-private` 打包页**。不得把这些权限加入 default `windows:["*"]`，也不得转交普通浏览器或小智设备。代价已知并接受：主窗口页面的漏洞同样能读到上面这些数据。详见 [`docs/integration/voice-private-acl.md`](../integration/voice-private-acl.md) 的 n26 节。安装后真实 Windows WebView2 的授权链仍需实际页面验证。

### 4.16 主窗口特权命令固定到本进程后端地址（2026-10-13）

主窗口导航到 `http://127.0.0.1:<端口>/` 后是 remote 来源，capability 的 `remote` 只能写 `http://127.0.0.1:*`，静态配置挡不住其他本机端口的页面。凡能取得桌面会话、改写配置或密钥、启停进程的命令（`get_desktop_session`、语音中心与内置语音的改写类命令、视觉/音乐/Agent 命令、第三方 MCP 管理、n26 起的私人记忆/插件/声纹/家居/长期记忆命令），必须在函数开头校验 `main` 标签与精确来源，失败即拒绝；新增此类命令照此处理，并加入 `main_window_commands_pin_backend_origin`（或 n26 的 `require_private_settings_window`）的清单。只给主窗口用的 capability 必须声明回环 `remote`，否则 Tauri 只对本地打包页授权、主窗口调用全部被拒。固件导入与推送仍只走 §4.13 的 `voice-private` 窗口。

### 4.17 家居控制与声纹识别改为用户开启（2026-10-14 用户决定）

- **家居控制（NV15）**：`HttpsHomeTransport::submit_once` 在生产中按 §4.10～4.15 门禁真实发送。开启条件全部由用户在私密窗口完成：HTTPS 地址、令牌、勾选白名单实体、明确同意语音控制（GRANT 绑定当前白名单与凭据版本，改任一项即失效）。语音只能生成审批；每个动作都要在私密窗口“家居控制审批”里批准，全局审批卡只提醒并打开私密窗口（主窗口的通用批准接口按设计拒绝 HomeControl）。至多一次 POST，结果未知不重发，批准前重新读取并核对状态与白名单。未用真实 Home Assistant 验收，界面提示先用无害设备试用。
- **声纹识别（NV16）**：用户登记至少两人后，每人另录三段测试音，`person_context::calibrate` 以“认成别人”的最高分加 0.05 为阈值（下限 0.5、上限 0.95），自检要求测试集内零误认、本人认对不少于三分之二；结果存于 `app_secrets`（`voice.speaker.calibration`），只对标定时的登记名单（编号与版本）有效，名单变化即失效。未标定时返回 `Unknown`，`may_authorize_action` 恒 `false`。只拿已登记的人互比，测不到陌生人；这是候选身份提示，**永远不能授权加工、家居或任何设备动作**，按人记忆仍需另行明确开启。

### 4.18 本机 Agent 与固件 OTA 改为用户开启（2026-10-15 用户决定；2026-10-25 开通 macOS/Unix 并支持自动读取 CLI）

- **本机 Agent（NV13）**：生产使用 `production_runner()`（遵循 §4.2～4.8 的不提权、不改 OS ACL、不把真实账号或长期密钥用于自动测试约束）。开启与执行条件全部显式：① 用户在语音中心“扩展 → Claude Code / Codex”打开开关并填写专用工作目录；CLI 路径可手动填写，也可**留空自动读取本机已安装的原生可执行文件**（`detect_cli` 仅扫描 PATH 与常见安装目录如 `~/.local/bin`、Homebrew `/opt/homebrew/bin`、`fnm`、`nvm`、`npm`、`pnpm`、`@openai/codex-<platform>/vendor/...` 与 `~/.local/share/claude/versions/<ver>`，只认真实原生二进制并拒绝 `.cmd`/`.js`/`.sh`/`#!` 脚本；当配置文件迁移到新电脑或升级版本导致旧路径不存在时，`resolve_with_detector` 自动适配当前设备的原生 CLI，而本机存在的不合法文件仍严格拒绝）；② 只在严格模式（语音中心顶部开关；值写在运行目录 `.env` 的 `MOSS_FUSION_STRICT`）下才提供工具，未开启时 `agent_codex` / `agent_claude_code` 不出现在工具列表里，服务端也拒绝；③ 每次运行都要在桌面审批卡单独批准一次，批准前不启动任何子进程；摘要绑定 provider、工作目录、**实际解析出的规范化 CLI 路径与版本**、配置 revision 与任务，批准时重算，不一致即拒绝；④ 批准后在后台线程运行，Windows 进程树受 Job Object 约束（挂起创建 → 加入 kill-on-close Job → 恢复），macOS / Linux 进程树受独立进程组约束（`process_group(0)` / `setpgid(0, 0)` + 下发任务 `stdin` 前二次授权复核 + 环境变量白名单 `CLI_ENV_KEYS` + 结束/取消/超时时 `kill(-pgid, SIGKILL)` 整组清理并轮询 `ESRCH`），运行中可在 Agent 卡停止，取消/超时/撤销设备授权/配置变化都会结束整棵进程树并写短错误码。
  **已告知用户并接受的剩余风险**：CLI 自身的沙箱挡不住“读取工作目录以外的文件”，Windows Job Object 与 Unix 进程组也不限制文件读取与网络。因此工作目录必须是可公开给模型看的目录，界面文案已写明；不得在任何文档或界面里声称已实现机密文件隔离。
- **固件 OTA（NV17）**：OTA 回包只在设备已允许**且**存在用户确认过的推送时才带 `firmware{version,url}`（`ServerConfig::firmware` 为 `None` 时一律不下发，严禁据模型自报 `device-id` 推送或下载）。导入与推送只在 `voice-private` 窗口由用户操作：只接受 ESP32 **应用**镜像（`0xE9` 头、偏移 32 的 `0xABCD5432`、1～5 段数字版本，单件 ≤16MiB、总计 ≤256MiB），文件落运行目录 `firmware/<sha256>.bin`，推送逐台确认、只对已允许设备、要求候选版本比设备上报版本新，有效期 24 小时。下载路由 `/xiaozhi/firmware/{token}.bin` 只认 32 位十六进制一次性令牌，并要求下载方地址等于这台设备最近一次 OTA 请求的地址，发送前重新读文件核对 SHA-256；取消或到期立即失效；重放、篡改、路径穿越、未允许设备一律 404。**下载完成不等于刷写成功**，只有设备之后上报的版本等于推送版本才记为“已升级”。真实刷写需另行授权，并具备串口重刷兜底。

## 5. 测试要求

测试隔离、`FakeGrbl` 字节级校验、SHA-256 重算、`0x18` 优先级和结果状态见 [`AGENTS.md`](../../AGENTS.md)。

## 6. 文档和提示词同步

- 改工具名、参数、注册开关、默认连接方式或确认规则时，同步 [`docs/integration/xiaozhi-prompts/latest.md`](../integration/xiaozhi-prompts/latest.md) 并新增日期快照；控制台正文 ≤2000、知识库 ≤200 字符（空格与换行计入）。
- 改路由、字段、错误码时同步 [Web API 契约](api-contracts.md)；改表结构时同步 [数据库结构](db-schema.md)；功能可见变化同步 [功能字典](feature-glossary.md)。
