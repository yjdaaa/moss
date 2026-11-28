# 开发者安全入口

改高风险代码前先读这页，再进入源码、测试和相关文档。这里是入口，不替代代码里的门禁。仓库级规则见 [`AGENTS.md`](../../AGENTS.md)。

## 1. 先确认运行时现实

- MCP 工具由 `laser_tools::ToolCatalog` 登记、`laser_mcp::dispatch_tool_call_with` 分发；小智连接由 `laser_host::HostApplication::spawn_mcp_bridge` 在 `MCP_ENDPOINT` 有效时启动。桌面端与 `moss-host serve` 走同一条路径。
- 目录共 10 个工具（常规 9 个：8 个核心激光工具 + `open_app_tool`；严格模式与内置语音仅列 8 个激光工具）。`command_execution_tool` 只在 `MOSS_ENABLE_DEVELOPER_COMMANDS=true` 时进入列表并允许执行；`/v1/capabilities` 的 `command_execution` 字段反映该开关。
- 激光工具目录里没有摄像头、Home Assistant、LaserGRBL GUI 自动化工具，提示词不得把它们写成激光工具。内置语音另有视觉（NV12）和 Home Assistant（NV15）工具；HA 控制默认关闭，由用户在语音中心“隐私与家居”同意后开启、每个动作逐次桌面批准，见 §4.4。
- 底层收发与重复旧入口（`laser_grbl_tool`、`laser_network_grbl_tool`、`recommend_laser_params_tool`、`generate_text_laser_task_tool`、`start_tuned_job_tool`）已于 2026-11-28（r76）移除，按名字调用返回明确的“已移除，请改用对应工具”错误（`tool_removed`），不触达后端。不对外开放任意 G-code 单条下发；文字与基础图形预览统一经 `laser_workflow_tool(action=preview)` 转发 `/api/text-task/generate`，图片素材须先从网页、桌面端或手机上传到 MOSS 上传目录后再走 workflow（r77 起语音 `laser_workflow_tool(action=preview)` 支持 `image="latest_upload"` 引用最近 30 分钟内上传的最新图片，绝不接受任意路径，预览与发送门禁不变）。
- 设备传输只有串口与 Telnet。没有 HTTP `/command`、`[ESP800]` 或 WebSocket 设备通道。

## 2. 高风险类别

以下改动默认按高风险处理，先读本页第 3 节，再读对应源码与测试：

- 完整发送链路：`laser-api` 的 `launch_send_job_for`、workflow `confirm_send`、材料矩阵 `send`、`/api/device/framing`、`/api/device/pointer-fire`（低功率点射，与走边框共用 `run_short_device_job`）。
- 设备层：`laser-device` 的握手、预检、`JobWorker` 流控、实时字节、S 值换算、SD 脱机上传与监控。
- 安全动作：`laser_tools::execute_laser_safe_action` 与 `/api/safe-action`，包括 `unlock`、`home`、`set_origin`、`laser_off`、`absolute_move`、`relative_move`、`emergency_stop`、`soft_reset`、进给/功率倍率，以及只对走边框作业的 `pause` / `resume` / `stop_framing`（2026-11-18）。
- 退出路径：`laser_host::ShutdownCoordinator`、panic 钩子、控制台关闭钩子、Tauri 关闭事件（含 macOS `RunEvent::Exit`）与托盘/菜单栏退出。
- 本地命令与桌面自动化：`command_execution_tool`（30 秒超时）、`open_app_tool`（Windows 经 `ShellExecuteW` 直调系统关联而不经过 `cmd /C start`，禁止向 `.cmd`/`.bat` 传参并对非 `.exe` 目标拦截 `cmd.exe` 元字符；macOS `/Applications/*.app` + `open -a <app> -- <payload>` 10 秒超时，拒绝 NUL/控制字符）。
- 局域网访问：`AuthManager`、`/v1/auth/*` 与公开路由白名单。
- 浏览器来源校验（CORS）：`laser-api` 的 `is_allowed_origin`、`handle_http_from`、`MOSS_ALLOWED_ORIGINS` 解析（`laser_runtime::config::parse_allowed_origins`）。不得恢复 `Access-Control-Allow-Origin: *`，不得把来源校验改成前缀匹配；前端开发端口（`5173/5177/5178`）仅在 debug 构建的回环请求中允许，release 构建默认拒绝；回环连接的 `Host` 白名单（`is_allowed_loopback_host`，防 DNS 重绑定）不得放宽成任意主机名；回环请求免鉴权，所以跨站网页的 POST 必须在路由前以 403 `origin_not_allowed` 拦下（见 [api-contracts.md](api-contracts.md) §1.6）。

不要用 `command_execution_tool`、桌面自动化、HTTP 逐行发送或前端直连来绕过 workflow 的确认门和在线检查。

崩溃钩子只用现有裸句柄发 `0x18`。跳过崩溃线程自己持有的作业表，其他线程持锁时共用约 50 毫秒的重试预算，超时记日志并返回错误，可读取的作业照常尝试急停。正常退出及信号线程走完整停机，包括任务落库收尾。

## 3. 完整发送的门禁

顺序与错误码以 `launch_send_job_for` 为准，任一失败都不写入加工指令：

0. **严格模式授权**（`MOSS_FUSION_STRICT=true` 时，技术书 §6.2；默认关闭，关闭时本步不读取任何数据、不改变判定）：**开关在语音中心顶部**——点开需二次确认；开启与关闭都立即生效，并把值写入运行目录 `.env` 的 `MOSS_FUSION_STRICT`（保留其他行与注释，原子写）。关闭会同时更新启动快照与运行时值（`set_strict_from_env` + `set_strict_mode`），不存在“要重启才生效”的中间态；语音链路切换（官方 ↔ 内置）不改严格模式。唯一检查点 `require_r3_authority`（`crates/laser-api/src/approval/strict.rs`）。桌面会话放行；小智桥接 / 局域网配对 / 网关只创建审批并返回 HTTP 202 `approval_required`，不连接设备；本机回环无凭据返回 403 `strict_mode_blocked`；以审批身份执行时（不论严格模式开关）按当前要素与审批保存的连接重算摘要并单次消费，执行使用审批保存的连接参数。无本地数据库时 503 `approval_unavailable`（失败即拒绝）。同一检查点也接在 `/api/safe-action` 的 R3 动作（`laser_contracts::safe_action_requires_approval`：除查询、急停、软复位、关激光、倍率复位与降低外一律视为 R3）与 `/api/device/framing`。`/api/device/pointer-fire` 不进审批，严格模式下只放行桌面会话（`require_desktop_when_strict`，见 §3 其他固定规则）。停止类动作永不追加条件。
1. HTTP 层要求 `confirmed=true`，否则 400；`laser-mcp` 在转发前同样检查。
2. `authorize_send`：记录处于 `pending`；重新读取物理 G-code 文件计算 SHA-256 并与冻结值比对；存在有效 workflow 快照或 calibration SHA。改材料、厚度、文件、策略、参数或连接目标后必须重新预览。
3. 同一设备已有在途作业：409 `device_busy`；宿主正在关闭时拒绝新作业并返回 503 `host_shutting_down`。刚建立且未写入指令的连接直接释放。急停与软复位仍可为明确指定的设备建连发送。
4. 打开传输失败：`device_unreachable`；打开连接后立即登记活动通道与取消令牌（`framing`、`pointer-fire` 与独立 `safe-action` 的 `home`/`unlock`/移动同口径），覆盖握手、`$CD` 与预检全窗口。
5. 有界握手（3 秒）：发 `$$` 并消费到终止 `ok`，要求 `$32=1`，读取 `$30`、`$10`；失败 `device_not_ready`。握手无响应时提示可能正在跑 SD 脱机作业。
6. `$CD` 读取 FluidNC 配置（必须收到终止 `ok`，超时排空并按字段叠加覆盖 `$$` 行程而不擦除其余轴）；旧 GRBL 返回 `error:3` 时静默跳过；未排空时拒绝续发顺序指令。
7. 物理 G-code 二次读取、SHA-256 与行程复核优先于 `$X` 解锁；续雕预检采用枚举区分 `<Alarm>` 与 `<Unknown>`，`<Unknown>` 等非报警态禁止自动 `$X` 解锁。自适应预检要求 `$30` 为有限正数并按 `$30/1000` 换算 S 值，发 `?` 要求 Idle，拒绝 Run/Jog/Home/Hold/Door/Sleep/Check/Alarm/Unknown、SD 运行标记及 `Pn:` 含 `E`/`F`/`D`；失败 `device_preflight_failed`。
8. **任务范围与快照冻结**：`build_send_preflight` 严禁在缺少冻结 SHA 时现算磁盘文件哈希，必须有 `confirmation_snapshot`/`artifacts` 冻结的 64 位 SHA-256（老数据须重新预览生成）。计算激光头走过的 X/Y 范围（`laser_core::gcode::motion_extents`），宽或高超限返回 400 `job_exceeds_travel`（`/api/device/framing` 同样复核）。行校验剥注释、丢空行；超过 254 字节或含非 ASCII 拒绝整个文件（`gcode_line_invalid`）；空文件拒绝（`gcode_empty`）。
9. 流式发送窗口内每次写入前重查取消令牌；完成后保持作业占用，`G4 P0` 同步 ACK 与后续 Idle 都通过才标记 `completed`。断线、复位横幅（`Grbl `、`[MSG:INFO: Restarting]`、`[VER:`）、报警或取消路径本身报错（此时必须记为 `failed`、`can_resume=false` 并尽力补发 `0x18`，绝不伪报安全停机）都是“执行结果未知”，不自动重发。故障与取消同时到达时保留原故障。续雕模态头和运行中的 SD 作业遇主动取消时，先发 `!` 等待停稳报告，再发 `0x18`。
10. `cancel` 只停止后续发送并走取消序列；`0x18` 走实时通道，不排队；中途取消仅采信启用 `use_line_numbers: true` + `N` 顺序行号下设备在 `Hold:0` 静止状态直接汇报的 `Ln:K` 边界（已完成 `K-1` 行，禁止用 ACK、`Bf:` 占用数或异步 `Idle` 估算断点，不能证实则 `can_resume=false`。续雕后再次取消也必须有本轮的新完成行证明），并在 `resume` 时按结构化连接标识锁定原目标与开工坐标系 `origin_mpos`；写入成功不证明物理停机。`0x18` 不替代硬件急停。

其他固定规则：

- TCP 和串口共用有界读行。连续拼接半行满 500 毫秒后向上层返回读超时，并保留已读取的半行。已经开始的底层读取仍由传输自身的单次超时约束。上层可继续检查取消位和各阶段的截止时间。
- 生成、预览、推荐参数、反馈调参、走边框预览 G-code 都不等于启动机器。例外是 `/api/device/framing`：它真实驱动机器沿外接矩形运动，改动时按发送链路对待。2026-10-14 起默认 `power=0` 不出光（弱光需显式传 `power`），与完整发送相同做同设备占用检查、握手与 Idle 预检，运行期间登记为活动作业 `framing-…`，顶栏急停与带 `job_id` 的 `emergency_stop` 可停止；失败或被停止后保留为“待确认停止”目标。语音 / 小智只能经 `laser_workflow_tool(action=frame)` 按预览走边框（2026-11-18，技术书 r51）：范围只取预览记录的 `frame_mm`、服务端锁死 `power=0`（带非 0 功率直接拒绝）、必须 `confirmed=true` 并先过与 `confirm_send` 相同的状态与物理 SHA-256 门禁；不得开放让模型自填坐标或功率的走边框入口。走边框途中可 `pause`（`!`）/ `resume`（`~`，须确认，严格模式 R3）/ `stop_framing`（先 `!` 再取消、停稳后 0x18），只作用于 `framing-…` 作业，正式加工不得用这套暂停（r52）。
- `laser_safe_action_tool` 中 `status`、进给/功率倍率，以及走边框途中的 `pause` / `stop_framing`（只让机器停下，2026-11-18）不需要确认；其余动作必须 `confirmed=true`（或 `dry_run=true` 只返回计划），走边框的 `resume` 同样必须确认。2026-11-27（技术书 r73）：进给/功率倍率只能命中正在运行（未取消）的加工任务并经原任务通道发送，无运行中任务或显式目标不匹配直接拒绝（`no_active_job` / `override_target_mismatch` / `override_target_ambiguous`），绝不新开连接；严格模式下提高倍率需桌面审批，批准执行当刻重新核对原任务仍在运行（原任务已结束返回 `no_active_job` / `override_job_ended`，不向空闲控制器或下一件发倍率字节）；前端监视区显示状态报告 `Ov:` 倍率，非 100% 时在开刻确认框提示。急停与停止路径不受影响。
- 两点对位（2026-11-16，[api-contracts.md](api-contracts.md) §3.13a）：`/api/registration/read-position` 只走 `status`（实时 `?`），丢弃请求里的运动字段，不得改成会发 G-code 行的实现；点动沿用 `/api/safe-action relative_move` 与其确认 / R3 授权。`/api/registration/preview` 只生成：原刀路先按冻结 SHA-256 核对，变换结果是新文件、新 SHA 的 `preview_ready` workflow，发送走完整门禁；变换器遇到机床坐标、坐标系切换、英制、非 XY 平面、`$` 命令等一律拒绝而不是猜测。新任务以第 2 个标记点为开始位置（`G92 X0 Y0`），前端生成前复核激光头仍在该点，软件无法保证开刻时激光头没被挪动，确认框的“对刀起点”写明这一点。画板任务同样可对位（2026-11-16，技术书 r42）：结果带画板视图，发送仍走画板确认框、`confirmed=true` 与全部门禁，内容指纹换成新刀路 SHA，不绕过重复发送防呆。
- 低功率点射（2026-11-16，技术书 r42，[api-contracts.md](api-contracts.md) §3.13b）：`/api/device/pointer-fire` 会真实出光。不变量：① 只接受工作台身份，语音 / MCP 桥 / 网关 / 审批重放不论严格模式一律 403 `pointer_fire_not_allowed`，不得加进 `ToolCatalog`、`laser_safe_action_tool` 或任何语音工具；桌面版（启用了桌面会话）里不带会话的本机回环一律 403 `desktop_session_required`（r43）；② 严格模式下只有桌面会话可用，不创建审批；③ 功率上限 S10（0~1000 标尺 1%，按 `$30` 换算）、时长上限 200 ms，越界拒绝不截断，必须 `confirmed=true`；④ 与走边框相同的占用检查、握手、Idle 预检与活动作业登记（`pointer-…`），失败或取消补发 0x18 并保留为待确认停止目标；⑤ 必须用 `M3`（`M4` 原地不出光），关光行 `M5 S0` 与开光行一起写入控制器缓冲区，不得拆成两次请求或改成“等用户松手再关”。前端默认关闭，每次点射都弹确认框，本页第一次附护目镜与反光提醒。不得把上限放宽来“看得更清楚”。
- 网络完整任务只走 Telnet；SD 脱机模式的 HTTP 只用于把文件上传到 SD 卡。
- SD 上传后核对固件返回的文件哈希；固件不返回哈希（未启用 HashFS）时**拒绝 `$SD/Run`**，任务记为 failed。软件比对通过也不能当作实机验收。
- SD 取消期间持续按脱机监控处理，即使状态报告已去掉 `SD:` 字段。`<Alarm|…>`、`ALARM:N`、迟到的 `error:N`、复位、断线或未能确认停稳均记为 `failed`，保留故障原因并尽力发送 `0x18`。
- 不猜串口号、设备 host、文件名、材料、厚度、密码、token 或用户私有路径；`resolve_connection_spec` 只在参数缺失时回退到 `GRBL_DEFAULT_PORT` / `LASER_NETWORK_HOST`，且忽略含 `<` 的占位值。
- 局域网配对 token 哈希与固定码哈希在 `AuthManager` 的 `state` 锁内同步持久化到 `settings.json`（写盘失败中止变更，支持全部撤销；临时配对码只在内存）；配对码只允许回环签发，启用桌面会话时另须校验 `X-Moss-Desktop-Session`（403 `desktop_session_required`）。

## 4. 当前边界与已知未接线项

- NV9 第三方 MCP 已接入桌面内置语音：配置命令只能来自主窗口当前实际回环来源，不提供 HTTP 管理接口；stdio 仍具有当前用户系统权限，不是沙箱，但子进程在 Unix 下通过独立进程组、Windows 下通过 `CREATE_NO_WINDOW` + `JobObject` 约束整棵进程树，HTTP 传输禁用系统代理（`.no_proxy()`）并将 `localhost` 固定解析至 `127.0.0.1`。工具默认 Ask，不因激光严格模式关闭或第三方声明只读而免批。名称、64 工具预算、停用状态在列表和调用处校验；来源会话、凭据／配置／schema 与参数绑定单次审批，改配置或撤销设备使未消费审批失效。执行前审计失败不放行；执行后写失败、超时或掉线不自动重发。详见 [第三方 MCP](../integration/moss-external-mcp.md)。

改相关代码时先核对这些现状，不要按文档或提示词想当然：

- `laser-api` 的文字、图片、G-code workflow 预览，以及 workflow / 标定矩阵发送和断点恢复，都会调用 `laser_core::safety::evaluate_material`。PVC 与明确危险材料直接返回 `material_safety_blocked`；仅写“塑料”等成分不明描述时要求澄清。小智提示词仍负责对话层确认与实物复核，后端也会独立拒绝，不依赖提示词是否遵守。
- 画板、图片与文字预览都按机器幅面（r78 起按“请求显式 → 后端机器档案 `laser_machine_profiles[target_key].manual_travel` → 记住的固件行程 → 默认 100 × 100 mm”取值）限制尺寸；**发送前仍按当场从固件读回的实时行程复核刀路范围（`check_job_fits_machine` / `job_exceeds_travel`），后端保存的手填行程只决定界面与 AI 的默认幅面与能力摘要，绝不能绕过发送前固件行程拦截（r65 / r68 / r78）**。生成的文字任务默认以激光头所在位置为木牌（图案）中心（`G92 X<半宽> Y<半高>`），图片任务以激光头位置为图片左下角。
- 后端机器档案（`settings.json::laser_machine_profiles`）与全局走边框形状（`laser_framing_shape`）（r78）：写入走 `/api/settings` 的操作者设置门禁（`require_operator_settings_access`，仅允许经来源校验的桌面主窗口修改；局域网只读、语音、MCP 桥与无桌面会话回环一律 403）；非回环未鉴权调用 `GET /api/system/info` 时 `machine_profiles` 返回空对象且 `verified_connection_target_key` 返回 `null`，避免泄露串口号或局域网控制器地址。语音 `workflow(action=frame)` 在 `laser_framing_shape="hull"` 时只使用预览冻结的 `artifacts.frame_hull_mm`（并经 `framing_outline` 校验与 R3 摘要绑定），绝不信任请求自带的 `outline`，且服务端继续锁死 `power=0`。
- 经审批执行的动作在消费审批的同一事务里先原子写入 `phase=running`，发送入口 `mark_running` 绑定 `job_id` 失败时拒发并写入失败终态；进程重启后，恢复逻辑把所有仍处于 `running` 或缺失终态阶段的 `executed` 审批标为失败、`error_code=result_unknown`，workflow 同时记为 failed，任何一方都不会自动重发（A-33）。
- 内置语音（NV4）以 `CallerContext::Voice` 调用工具。运动与出光（R3）**跟随 `MOSS_FUSION_STRICT`**（2026-10-07 用户决定）：开启时只生成审批（来源 `voice`），关闭时与官方接入点相同，模型带 `confirmed=true` 即执行——此时唯一的确认是系统提示要求模型先复述参数、等用户口头确认，没有桌面批准。2026-11-06 起（技术书 r18）AI（语音、小智接入点、网关）生成的每个预览都会在桌面弹出预览窗（`AiPreviewCenter`），用户可在窗内勾选现场确认后“同意开刻”或取消；严格模式开启时只能在界面同意，关闭时口头确认后 AI 仍可 `confirm_send`。文字生成工具的 `confirmed + send_after_generate` 不再顺带发送，只生成预览。R4 与未分级工具（命令执行、打开应用、快捷键、打开网站）**不论开关都拒绝**（`HostBackend::always_deny_r4`），返回 `tool_not_allowed` 并记审计 `voice:<设备>`；工具清单与严格模式下的接入点相同（`laser_host::voice_tools_list`）。停止类动作照常立即执行。
- 语音抢跑闸门（2026-11-13 n86，规则见 [`AGENTS.md`](../../AGENTS.md)）：`moss-voice-server::conversation` 的 `TurnGate` 允许对话任务在确认用户说完之前先开跑；大模型发起的每批工具调用（含退出工具与 n144 `voice_timer_tool` 的 `set`/`cancel`）执行前都等 `Turn::wait_release` 放行，`hold_llm` 轮次（对正在播的回答插话）连大模型请求也等；会话放弃这一轮时任务原地等待被中止，`ToolLatch` 保证“已开始执行工具”与“作废重来”只发生其一，同一工具不会被执行两遍。新增由大模型触发、有副作用的执行路径必须排在闸门之后；只有整句停止快路径（§4.1）可以不过闸门。
- 记住的连接（n73，`laser_verified_connection`）只来自工作台身份的真实握手成功；只在调用方没给任何连接参数时补上，补全发生在门禁之前，审批与发送前握手、SHA 重算照旧绑定补全后的目标。不得让语音或接入点自己的检查改写它。
- 临时参数（n73，`manual_params`）只影响这次生成的刀路，范围与反馈调参相同（功率 1~100%、速度 100~6000 mm/min、遍数 1~5），显示在预览里，发送仍须确认；不得放宽范围或绕过材料安全门禁。
- 统一工具清单（2026-11-28 r76 合并原 n72 精简/完整双清单，移除 `mcp_full_tool_list` 开关）：`tests/golden/mcp_tools.json` 为唯一 schema 来源，8 个激光工具使用面向意图的中文说明；修改工具说明时不能把需要 `confirmed=true` 的动作写成“不用确认”（急停在执行层要求 `confirmed=true`，只是不需要先问用户）。
- 工具开关（NV8，`mcp_disabled_tools`）只能减少可用工具：关闭的工具在接入点与内置语音的清单和调用两处都拒绝（`tool_disabled`，不触达后端）。`laser_safe_action_tool`（停止、急停）列在 `ALWAYS_ENABLED_TOOLS`，后端与语音中心都不允许关闭；新增停止类工具时须同时加入该列表。内置语音专用的计时提醒工具 `voice_timer_tool`（n144，不涉激光、无审批卡、仅存内存重启丢失、到点只向发起设备播报、设备离线/撤销记 `DiagSource::Xiaozhi` 事件绝不改投他机、对话中不打断）默认开启，也可经同一 `mcp_disabled_tools` 关闭（语音中心仅在内置模式显示）。
- 小智接入点的工具分级在 `laser_contracts::tool_risk`（R0 只读、R1 生成、R2 降低风险、R3 运动或出光、R4 永不开放）。严格模式下接入点对 R4 与未分级工具返回 `tool_not_allowed`，R3 交给执行层生成审批；新增 MCP 工具时必须同时补分级，否则 `laser-mcp` 测试失败。
- 严格模式下桌面端通过 `get_desktop_session` 携带 `X-Moss-Desktop-Session` 放行；不带桌面会话头的普通本机回环请求返回 403 `strict_mode_blocked`。
- `audit_log` 自 2026-10-06（P1-08）起由审批服务与接入点写入：审批创建 / 批准 / 拒绝 / 过期 / 执行 / 失败、模型自填 `confirmed` 被删除、R4 拒绝、严格模式拦截。创建、批准、执行三步与审计在同一事务中，审计写不进去就拒绝（R3 不放行，状态不变）；拒绝、过期、失败与停止类动作不依赖审计。
- `jobs` 表已建但运行时无写入方（`auth_pairings` 等 7 张废弃表已在 schema v18 删除）；进度只在内存中的作业注册表与 workflow / calibration 记录的 `send_result` 里。
- S1-09 Android 实际来源（`http://tauri.localhost`）必须经安装设备核验，不得用 APK 构建通过冒充实机来源验收。

### 4.1 语音授权与停止修补

设备 token 校验后还需注册当前 `device/client/session`。撤销使所有旧会话标记永久失效，会话循环检查后关闭连接；重新允许只能建立新会话。工具执行入口再次检查，尚未消费的该设备审批同事务过期，预检或排队期间失效的语音发送不启动加工。已经执行的指令不因撤销自动撤回，桌面停止能力始终保留。新设备在 `pending` 态跑过开机 OTA 后被桌面允许时，仅在同时满足四项条件时放行无 token WebSocket 直连（n133）：① 状态为 `allowed`；② `client_id` 非空且与绑定记录严格一致；③ `decided_at` 存在且距当前不超过 7 天（`TOKENLESS_GRACE_SECS`）；④ 允许后尚未完成过已绑定的开机 OTA（`info_at` 为空或早于 `decided_at`，一旦下次重启走 OTA 签发 token 即关闭宽限期）；携带无效或过期 token 的连接照旧拒绝，且无 token 会话不下发拍照识图能力。`auto_allow_devices` 默认关闭，只允许在经来源校验的桌面主窗口经局域网信任确认后开启，且只对开启后新连入的陌生设备生效，不自动放行存量 `pending` 记录和已 `revoked` 设备。移除只适用于 `allowed` / `pending` 设备；`revoked` 设备不可移除，以免绕过撤销。

停止快捷通道只接受最终识别的完整短句，不因中间词“停止”抢先执行；仍在大模型前处理。命令词模型（n101 / n114）只在思考中新的一阵声音里听到停止词时不等识别直接急停；刚说完那句尾音里报出的停止词交给识别文字判断，带否定（“别停”“不要停”）或“暂 / 等”的不算；“暂停”不是停止类命令词（它对应可恢复的 `pause`）。工具开跑后、开口前用户说“停”照样识别并急停；工具粗筛永远保留 `laser_safe_action_tool`。无活动任务时需已明确配置的目标与现有 `confirmed` 契约，多任务未指定目标不随机选择；状态查询失败不等于任务失败，也不等于停机。界面持续重试并保留急停，请求写入成功只显示“已发送，停止未确认”。

本地判停在独立工作线程运行，事件循环不等待 IPC；队列和并发线程有界，排队到响应超时关闭异常会话，旧录音轮次的结果丢弃。不能宣称被阻塞的模型计算一定已被强制终止，也不能以软件通过代替实体急停或实机性能验收。

### 4.2 本机 Agent（NV13）与离线沙箱边界

- **开启与审批条件**：生产使用 `production_runner()`。① 用户在语音中心“扩展 → Claude Code / Codex”打开开关并填写专用工作目录（CLI 路径可手动填写或留空由 `detect_cli` 自动读取本机原生二进制，拒绝 `.cmd`/`.js`/`.sh`/`#!` 脚本；迁移后旧路径不存在时由 `resolve_with_detector` 重新适配，不合法文件仍严格拒绝）；② 只在严格模式（`MOSS_FUSION_STRICT`）下暴露工具，未开启时 `agent_codex` / `agent_claude_code` 不出现在工具列表里且服务端拒绝；③ 每次运行须在桌面审批卡单独批准，审批前不启动子进程，摘要绑定 provider、工作目录、实际规范化 CLI 路径与版本、配置 revision 与任务，批准时重算，不一致即拒绝。
- **进程树隔离边界与剩余风险**：Windows 进程树受 Job Object 约束（挂起创建 → 加入 kill-on-close Job → 恢复），macOS / Linux 受独立进程组约束（`process_group(0)` / `setpgid(0, 0)` + `stdin` 下发前二次授权复核 + 环境变量白名单 `CLI_ENV_KEYS` + 结束/取消/超时时 `kill(-pgid, SIGKILL)` 轮询 `ESRCH` 整组回收），禁止自动启用提权沙箱或修改 OS ACL。**剩余风险**：CLI 自身与 OS 进程组均不限制读取工作目录外文件或网络，工作目录必须是可公开给模型看的目录，不得在文档或界面声称已实现机密文件隔离。
- **CLI 能力探测**：探测用固定参数，两个输出管道共用一次探测预算。Windows 的探测进程如果仍在运行且无法加入 Job Object，会尝试终止探测并返回无结果。
- **Docker 与 `model_proxy` 离线夹具（未接入生产）**：`scripts/agent-sandbox/test-container-isolation.ps1`、`moss-agent/src/docker_plan.rs` 与 `moss-agent::model_proxy` 仅作离线安全测试夹具。测试容器须保持 `--rm --pull never --network none --read-only --cap-drop ALL --security-opt no-new-privileges --pids-limit 8`、非 root 与唯一只读合成工作区挂载；严禁加 `--privileged`、`CAP_SYS_ADMIN`、`seccomp=unconfined`，严禁挂载 MOSS 目录、用户主目录、Docker socket 或真实长期密钥。详见 [本机 Agent Docker 隔离](../integration/agent-sandbox-docker-security.md)。

### 4.3 本地音乐（NV14）隔离边界

音乐默认关闭且无目录，不自动扫描任何个人文件。只在用户显式启用并保存本地目录后，由桌面主窗口主进程按 8 层、10,000 曲目 / 5,000 目录 / 50,000 项限额扫描安全根，跳过敏感子目录（Windows `AppData`、macOS `Library`/`.Trash`/`.ssh` 等），拒绝 UNC、软链/reparse、敏感系统根（含 Windows 系统卷根与 macOS `/System`、`/Library`、`~/Library`、`/Applications`、`/Volumes` 顶层挂载根，仅豁免系统卷根 firmlink `/var`、`/tmp`）。受信索引生成 `track_id`，重名候选仅显示非路径索引 ID 与编号（不明确选择不播放），模型不得提供路径/URL 或与其他工具混用。读取前按打开文件实际句柄（`GetFinalPathNameByHandleW`、`O_NOFOLLOW` + `fcntl(F_GETPATH)`、`/proc/self/fd`）与 size/mtime 复检；`voice_music_status`/`save`/`pick_directory` 只授予桌面 `main` 窗口且不自动扫描。

### 4.4 家居控制（NV15）、凭据备份脱敏与声纹识别（NV16）

- **家居开启与审批（NV15）**：`HttpsHomeTransport::submit_once` 生产发送条件全部由用户在语音中心“隐私与家居 → 家居”完成（HTTPS 地址、令牌、勾选白名单实体、明确同意语音控制，`GRANT` 绑定白名单与凭据版本，改任一项失效）。语音只能生成审批，每个动作须在“家居审批”逐次核对批准（通用审批接口按设计拒绝 HomeControl，只能走 `voice_home_approval_approve`）；未用真实 Home Assistant 验收。
- **Schema 校验与单次提交闭环**：`moss-home::voice_home` 校验 HA 状态与服务 schema，拒绝未勾选实体、`unknown`/`unavailable`、模型篡改增加属性及高风险域自动放行；对外状态仅保留最少数值，丢弃 PIN/密码/token 与嵌套属性。只读 GET 与控制 POST 共用 `moss_home::home_readonly::pinned_client`（私网 IP 检查、一次性固定解析、HTTPS、无代理、不跟随重定向）。每个审批绑定 endpoint、凭据与策略 revision、设备许可、动作与参数摘要，消费后**至多一次 POST**；敏感改动、取消与 POST 共享 `send_fence` 及 `BEGIN IMMEDIATE` CAS 串行化，最终状态与审计同事务入库。回读（最多 3 次 GET，总预算 3 秒、单次上限 900ms）期间超时、HTTP 错误、收到与否不明或设备许可撤销时，**一律持久记为 `result_unknown` 且绝不自动重发**。
- **凭据封存、导出限制与日志审计**：Windows 用当前用户 DPAPI（`dpapi-user-v1:`），macOS 用登录钥匙串主密钥派生 Encrypt-then-HMAC-SHA256 信封（`keychain-user-v1:`）。仅允许从经 `require_private_settings_window` 校验的主窗口导出/导入固定白名单 `moss-home-profile-v1`（不含 origin、媒体库 ID、许可或启用标志），禁止打包 SQLite/WAL/SHM 或迁移原始密文（`home_backup_audit` 对两类前缀标记不可分享）。恢复须二次确认并先原子撤销旧 token、grants 与待审批并将实体置为禁用；遇到跨账户密文仅允许受限启动撤销迁移。历史备份审计由 Rust 原生文件框选单个本地非链接普通文件（≤64MiB），只输出类别/字节数/禁分享标志；`moss_voice::redact::redact_line` 做重脱敏，不替代完整 DLP。
- **声纹识别（NV16）**：至少两人各录三段测试音，`person_context::calibrate` 以跨人最高分 + 0.05（下限 0.5、上限 0.95）为阈值，要求测试集零误认且本人命中 ≥2/3；结果存于 `app_secrets`（`voice.speaker.calibration`），名单变化即失效；未标定返回 `Unknown`，`may_authorize_action` 恒 `false`。**声纹仅作候选身份提示，永远不能授权加工、家居或任何设备动作**。

### 4.5 主窗口特权命令、本地敏感 ACL 与固件 OTA（NV17）

- **主窗口精确来源固定与敏感 ACL**：主窗口导航到本机 `http://127.0.0.1:<端口>/`，capability 中仅静态配置 `remote:["http://127.0.0.1:*"]` 挡不住其他本机端口页面。因此三类特权命令均须在 Rust 入口校验精确来源：① `get_desktop_session`、常规语音启停/配置、导出与托盘命令由 `default.json` 授权并在函数内校验 `require_main_window` 或 `require_trusted_app_window`；② 本机 Agent、第三方 MCP、本地音乐、视觉扩展命令各自在 `capabilities/voice-{agent,mcp,music,vision}.json`（`windows:["main"]` + 回环 `remote`）授权并校验 `require_main_window`；③ 私人记忆、插件凭据、声纹、家居、长期记忆、固件、唤醒词与 USB 配网命令集中在 `capabilities/voice-private-main.json`（`windows:["main"]` + `remote:["http://127.0.0.1:*"]`），开头用 `require_private_settings_window` / `window_is_trusted_main` 校验，严禁放入 `default.json`（`windows:["*"]`）。新增主窗口特权命令须同步加入 `apps/desktop/src-tauri/src/lib.rs` 的 `main_window_commands_pin_backend_origin`（或 `require_private_settings_window` / `frontend/tests/voicePrivacyAcl.test.mjs`）检查清单；安装后 Windows WebView2 授权链仍需真实页面核验。
- **固件 OTA（NV17）、资源包推送与语音 sidecar（NV18）**：OTA 回包只在设备已允许且存在用户逐台确认的推送时才带 `firmware{version,url}`（`ServerConfig::firmware` 为 `None` 时不下发，严禁按模型自报 `device-id` 推送）。导入与推送、设备唤醒词推送仅在主窗口语音中心由用户逐台确认；唯一例外是“副屏预览 › 自动保持全屏画风”（默认关，开启时确认一次）：开启后 MOSS 可向尚未装好当前全屏画风包的在线彩屏小智自动推送画风资源包（沿用原唤醒词，加工中、有待批准请求、对话中不推，每台每天最多一次，n78）。固件只接受 ESP32 应用镜像（`0xE9` 头、偏移 32 的 `0xABCD5432`、1～5 段数字版本，单件 ≤16MiB、总计 ≤256MiB），落盘 `firmware/<sha256>.bin`，要求候选版本高于设备上报版本，有效期 24 小时。下载 `/xiaozhi/firmware/{token}.bin` 只认绑定推送的 32 位十六进制令牌。相同授权在下载中可重试，发送前复核最近一次 OTA 来源、设备许可、推送修订和镜像 SHA-256。取消、上报完成、到期、篡改、路径穿越或未允许设备一律 404。**下载完成不等于刷写成功**，只有设备随后上报目标版本才记为“已升级”；真实刷写需另行授权并具备串口重刷兜底。语音 sidecar 专用安装包（`scripts/voice-helper/`）因 `sherpa-onnx`/`espeak-ng` 完整许可与对应源码审计未闭环（且已暂停排期），**不得对外分发**；详见 [`docs/integration/voice-private-acl.md`](../integration/voice-private-acl.md) 与 [`docs/developer/voice-helper-license-audit.md`](voice-helper-license-audit.md)。

### 4.6 模拟联调轮次、测试时间窗与本机调试入口门禁（VA / VC，2026-11-27）

- **模拟轮次工具执行门禁（`moss_voice_server::sim_guard`）**：所有经文字直通注入、处于测试时间窗内或经数字音频对照注入的轮次均标记 `simulated = true`，不能代替真人做任何授权。拦截判断位于工具执行入口（`conversation::Turn::tools_future` 调用 `tools.call` / `device.call` 之前），不依赖提示词；即使严格模式开启，也在创建桌面审批之前直接拒绝，无法借由审批路径放行。一律拒绝的工具包括：激光发送、续雕、走边框、绝对/相对移动、回零、解锁、实时倍率与 `laser_safe_action_tool` 中除 `status` 以外的全部动作、校准矩阵发送、家居控制、本机 Agent（`agent_*`）、固件/唤醒词推送、设备重启、`play_music`（VE4：整体测试及模拟轮次期间不播放音乐以免干扰后续串行测试）以及 `voice_timer_tool` 的 `set` / `cancel`（n144：模拟轮次仅放行 `list`），并把拒绝原因如实返回大模型；预览生成、状态查询、设备信息读取以及音量/亮度/主题等设备自身无风险控制照常放行。
- **模拟轮次停止快路径与记忆隔离**：模拟轮次命中“停 / 停止”快路径时，先经 `laser_host::voice_has_active_laser_job` 实时核查是否存在活动激光任务；有任务（或探测不可用按保守策略处理）时绝不向激光机发送 `0x18` 急停，仅在记录中写入“模拟轮次的停止命令未执行”；确认无任务时按原快路径运行以验证响应链路。模拟轮次在 `voice_chat_logs`（`simulated = 1`，测试时间窗内另标 `bench_window = 1` 并显示“模拟（整体测试）”）与实时记录中显式标识，并由独立的 `Session::memory_history` 隔离——长期记忆、人物资料、声纹登记与用户档案提取全部跳过模拟轮次。
- **整体测试时间窗安全锁（VC1，`BenchWindowStore`）**：电脑外放音频经小智真麦克风回采时会被服务端视作真实音频输入，因此必须先通过 `POST /debug/v1/bench-window` 打开目标设备的测试时间窗（默认 30 分钟、硬上限 60 分钟）。时间窗开启期间，该设备的**全部真实音频轮次**强制升级为 `simulated = true`（受上述 `sim_guard`、停止快路径、记忆与声纹隔离同等保护），同时桌面语音中心展示醒目“整体测试中（物理动作已锁定）”标记与“立即结束测试”按钮（经主窗口 `voice_device_request({deviceId, request:"close_bench_window"})` 立即解锁）。时间窗在 5 类条件下自动关闭并写诊断事件：脚本主动关闭或用户点按钮、到达 `max_minutes` 超时、设备断开、调试开关关闭、24 小时调试令牌过期；脚本须在 `finally` 中兜底关闭。
- **数字音频注入对照与 AI 联调提示词边界（VC2 / VD6）**：`POST /debug/v1/devices/{device_id}/inject-audio` 仅用于对比外放与服务端直注识别差异，**必须同时通过回环/令牌鉴权且目标设备当前会话已打开测试时间窗**（否则返回 403 `bench_window_required`），其触发轮次恒为 `simulated = true` 且**绝不写入 `asr-corpus`**（与 VA 文字直通一致；仅外放经真实麦克风进入的轮次在开启语料采集时写入 `asr-corpus` 并由脚本标为 `label_source: "e2e-playback"`）。语音中心「记录与诊断 › AI 联调与测试」复制的提示词由纯函数 `buildAiBenchPrompt` 生成，绝不包含调试令牌、API 密钥、设备 token 或本机绝对路径，并禁止修改语料迁就测试或自动应用识别候选。
- **本机调试入口门禁（`moss_voice_server::debug_api`、`moss_voice_host::builtin::debug_inject`）**：入口位于语音侧独立 HTTP 监听，严格绑定 `127.0.0.1`（绝不监听 `0.0.0.0` 或局域网，与 `laser-api` 保持域隔离），在请求体提取前由中间件统一校验对端为回环地址、`Host` 属于回环白名单（防 DNS 重绑定）、拒绝携带 `Origin` 请求头的浏览器跨源访问，并限制并发与单路由体积上限。设置项 `builtin_local.debug_text_inject_enabled` 默认关闭，仅允许经来源校验的桌面主窗口（`voice_local_save` / `voice_device_request`，入口校验 `require_main_window`）在弹窗确认后热启停；开启时在 `<runtime>/debug/voice-debug.token` 以 `0600` 权限原子写入 32 字节随机令牌，关闭开关或开启满 24 小时自动作废并删除文件。接口采用常量时间 Bearer 令牌校验，且仅暴露受限调试操作（列出在线设备、按设备查近期轮次、文字注入、时间窗内数字音频对照注入、开关测试时间窗、查单轮明细、启停过程记录归档），不提供直调工具、改设置或下发设备命令的接口；局域网、MCP 网关与语音会话本身均无法调用。

### 4.7 识别（ASR）真实录音语料、本机测试录音与离线测评门禁（VB / VD，2026-11-27）

- **默认关闭与敏感主窗口 ACL**：`builtin_local.asr_corpus_capture_enabled` 默认关闭，不在 `voice_local_save` 暴露；采集开关、保留上限（默认 500 条 / 7 天，先到者先清理）、录音读取、人工标注与删除全部收口到敏感命令 `voice_asr_corpus_manage`，仅由 `capabilities/voice-private-main.json`（`windows:["main"]` + 回环 `remote`，严禁进入 `default.json`）授权，且命令入口强制 `require_private_settings_window(&window)?`。开启前须二次确认（后端同时要求 `confirmed: true`）；关闭开关时询问是否同步删除已有语料，关闭后 `audio` / `save_label` 返回 403 `asr_corpus_disabled`，但仍允许 `delete` / `clear` 清理旧语料。
- **本机整体测试真人录音与麦克风权限（VD2，`voice_bench_recordings_manage`）**：整体测试严禁使用本地 TTS 或系统朗读替代真人声音；桌面端经 WebView `getUserMedia` 录制 16 kHz 单声道 WAV，保存、试听、列出与删除全部收口到敏感命令 `voice_bench_recordings_manage`（仅 `capabilities/voice-private-main.json` 授权且入口强制 `require_private_settings_window(&window)?`，严禁进入 `default.json`）。落盘至 `<runtime>/voice-bench/recordings/<speaker>/<corpus-id>.{wav,json}`（Unix 权限 `0700`/`0600`，拒绝符号链接与路径穿越，前后端双侧校验时长 0.5–15 秒、音量不过低、不削顶）。说话人仅存标签，不涉及身份识别且绝不用于声纹；录音仅存本机，不上传、不联网，严禁进入 `diagnostics_bundle()` 与 `ProcessTrace` 附件；删除全部录音须二次确认（`confirmed: true`）。macOS 在 `apps/desktop/src-tauri/Info.plist` 声明 `NSMicrophoneUsageDescription`（“用于录制本机语音测试语料”），各平台权限被拒均给出系统设置引导。
- **网上真人语音缓存的来源区分、许可证与限量门禁（VE1–VE4，2026-11-27）**：由 AI 按一键提示词在本机运行目录缓存的网上真人语音必须放入 `<runtime>/voice-bench/recordings/online/`，脚本严格按目录标为 `source_type: "online-human"`（绝不混入用户自录 `human-recording`），同名 `.json` 必须完整填写 `text`、`dataset`、`license`、`source_url`（缺失或仅用 `.txt` 一律跳过，并按命令词保护规则过滤“播放/放首歌/音量/亮度/静音/关机/重启/停/退下/开始”等疑似指令句）。联网拉取仅限明确允许的公开许可数据（如 Mozilla Common Voice CC0、AISHELL-1 Apache-2.0、`sherpa-onnx` 仓库自带测试音频），严禁使用“非商用”或“禁止演绎”数据，遇登录/申请/条款限制直接跳过并说明、严禁绕过；单次补充 ≤200 条且总计 ≤300 MB，仅在独立临时目录下载音文且严禁运行下载内容里的任何脚本或程序，转换后仅将 `.wav` 与 `.json` 存入运行目录、绝不加入仓库或提交。整体测试默认 `--max-online 20` 并经 `.rotation.json` 轮换，预估超窗时按批正常关窗重开且每批复核同一设备会话（单窗硬上限仍为 60 分钟）。
- **数据隔离与最小暴露**：原始 16 kHz 单声道 WAV 与元数据只写 `<runtime>/asr-corpus/<date>/<turn-id>.{wav,json}`（Unix 权限 `0700`/`0600`，拒绝符号链接与路径穿越）；VA 模拟轮次与 VC2 数字注入对照无真实麦克风音频、一律不保存。两类录音语料均严禁进入 `diagnostics_bundle()` 诊断包与 `ProcessTrace` 附件，绝不用于声纹识别或身份判断。
- **离线测评命令词保护与零自动生效**：`scripts/voice-bench/asr-bench.mjs` 产出的同音纠错候选严格禁止把任意内容映射成停止/退出/确认/开始等保护命令词、禁止改写命令词本身、禁止包含 G-code / `$n=`，违规候选直接丢弃并在 `report.md` 注明；云端识别密钥从本机只读读取且绝不打印或写入归档，归档全量脱敏；所有候选只写 `suggestions.json`，绝不自动修改源码或 `settings.json`。

### 4.8 激光 AI 自主测试隔离、L1 只读白名单与 L2 真机空跑强制门禁（LA / LB，2026-11-27 / 2026-11-28）

- **L0 模拟机完全隔离（`scripts/laser-bench/laser-sim-bench.mjs`）**：每次运行在系统临时目录新建独立运行目录，覆盖 `HOME` / `APPDATA` / `XDG_DATA_HOME` / `LASER_MATERIAL_PARAMS_FILE` 并清除外部真实设备环境变量，仅启动独立的 `moss-host serve` 进程对接监听 `127.0.0.1` 随机端口的回环 GRBL/FluidNC 模拟器（`sim-grbl.mjs`）；绝不触碰用户日常运行目录、真实连接配置或任何真实设备。退出与信号清理阶段必须等待子进程退出后再清理临时目录。
- **L1 真机只读白名单与忙碌拦截（`scripts/laser-bench/laser-readonly-check.mjs`）**：仅访问本机运行的 MOSS 回环接口（拒绝任何非回环 URL），沿用现有鉴权规则、不新增绕过鉴权的入口。全部 HTTP 请求在发出前强制经 `assertReadOnlyRequest` 白名单校验，仅放行 `GET /api/{health,system/info,jobs/stop-targets,ui/runtime-config,ui/serial-ports}`、`POST /api/connection/check`（只发 `$$`/`$CD`/`$I`）以及 `POST /api/safe-action` 中**唯一合法的 `action="status"`**（禁止任何运动/功率/`confirmed` 字段）；所有发送、走边框、点射、移动、倍率、急停、软复位、解锁、回零、改设置等请求一律在网络调用前直接抛错拒绝。预检发现任何运行中或待确认停机任务（`/api/jobs/stop-targets` 非空）时立即中止且不碰设备；归档全量脱敏令牌、绝对路径、真实设备 IP 与串口名。
- **L2 真机空跑强制约束与开关门禁（LB2 / LB3，融合技术书 `r75`）**：
  - **单开关授权（用户 2026-11-28 明确决定）**：`laser_dry_run_enabled` 默认关闭、重启不继承；仅允许经 `require_operator_settings_access` 校验的桌面主窗口（`X-Moss-Desktop-Session`）在弹窗确认后开启（60 分钟自动过期关闭，随时可手动关闭）。局域网、语音、MCP、网关、审批重放、无桌面会话回环与携带 `X-Moss-Bench` 头的脚本一律 `403` 拒绝开关。开启该开关即授权本次 L2 空跑测试，不做逐步审批；**真机出光永远由用户手动执行**，急停 `0x18` 照常走实时通道不受影响。
  - **最底层强制改写与高风险快速指令拦截（`laser_device::DryRunTransport` + 发送预检 + SD 脱机上传 `sd_offline_upload_payload`）**：覆盖所有发往激光机的顺序写入出口（流式任务、已有 G 码上传、续雕模态头与正文、走边框、点射、点动、回原点、安全动作及 SD 脱机上传）。开启期间：① 所有 `S` 字改写为 `S0`，无 `S` 的 `M3`/`M4` 补 `S0`，任务开头补 `G21 G94 G1 M5 S0 F<折算上限>`（同时消除上电默认 `G0` 与残留 `G20`/`G93`）；② 所有 `F` 钳在折算上限内，`G0`（含 `G53 G0`）改写为 `G1 F<折算上限> S0`，裸 `G53` 与复位后无显式模态的坐标行自动补 `G1 F<折算上限> S0`；实时倍率字节照常放行，但下发 `F` 上限按 GRBL/FluidNC 最大进给倍率 200% 折半（`dry_run_max_feed_mm_min / 2`，可配范围 `300–5000`、默认 `2000` mm/min），保证 200% 倍率下实际物理速度仍不超过上限；③ 发送预检（`check_gcode_lines_for_dry_run`）直接拒绝含 `G28` / `G30`（回预存位置，按快速速度移动；`G28.1` / `G30.1` 仅存位置不运动，放行）、`G93`（反时间进给）、`G20`（英制单位）或外部固件文件/宏执行（`$LocalFS/Run`、`$Macro`、`$RM`、`$N0=`/`$N1=`、外部 `$SD/Run`）的任务；若传输层兜底仍遇到此类指令，替换为无运动注释 `; dry-run blocked <code>` 并记录带来源标签的告警事件；`$H` 机械回零按固件自设回零速度执行、保持放行；④ 打开连接时若空跑已开启，该连接锁存空跑策略直到关闭，防止任务中途到期或关开关导致半途出光；⑤ 行程与越界检查照常生效不放宽。
- **固件 `$22=0` 回零拦截（LB1）**：`laser_tools::execute_laser_safe_action_with_gate_cancel_and_dispatch` 与 `laser_device::DeviceConnection::home_cancellable` 在握手确认 `homing_enabled == Some(false)` 时直接返回 `homing_disabled` 拒绝，不发 `$H` 也不触发急停兜底；未报告（`None`）时保持原有行为。
- **过程记录标签隔离（LB4）**：`X-Moss-Bench: readonly|dryrun` 仅用于向 `ProcessTrace` 写入来源标记（`source` 与 `timeline.txt` 标签），**绝不授予任何权限**；空跑开启期间全部激光记录（含手动操作）统一附加 `dry_run: true` 与「空跑模式」。提示词由纯函数 `buildLaserBenchPrompt` 生成，不含令牌、绝对路径或设备地址。

## 5. 测试要求

测试隔离、`FakeGrbl` 字节级校验、SHA-256 重算、`0x18` 优先级和结果状态见 [`AGENTS.md`](../../AGENTS.md)。

## 6. 文档和提示词同步

- 改工具名、参数、注册开关、默认连接方式或确认规则时，同步 [`docs/integration/xiaozhi-prompts/latest.md`](../integration/xiaozhi-prompts/latest.md) 并新增日期快照；控制台正文 ≤2000、知识库 ≤200 字符（由 `scripts/check-repo-hygiene.mjs` 自动核验）。
- 改路由、字段、错误码时同步 [Web API 契约](api-contracts.md)；改表结构时同步 [数据库结构](db-schema.md)；功能可见变化同步 [功能字典](feature-glossary.md)。
