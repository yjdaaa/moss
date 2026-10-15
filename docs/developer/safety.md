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
- 退出路径：`laser_host::ShutdownCoordinator`、panic 钩子、控制台关闭钩子、Tauri 关闭事件与托盘退出。
- 本地命令与桌面自动化：`command_execution_tool`、`open_app_tool`、`shortcut_key_execution_tool`（Windows 上通过 PowerShell `SendKeys`）。
- 局域网访问：`AuthManager`、`/v1/auth/*` 与公开路由白名单。
- 浏览器来源校验（CORS）：`laser-api` 的 `is_allowed_origin`、`handle_http_from`、`MOSS_ALLOWED_ORIGINS` 解析（`laser_runtime::config::parse_allowed_origins`）。不得恢复 `Access-Control-Allow-Origin: *`，不得把来源校验改成前缀匹配；回环连接的 `Host` 白名单（`is_allowed_loopback_host`，防 DNS 重绑定）不得放宽成任意主机名；回环请求免鉴权，所以跨站网页的 POST 必须在路由前以 403 `origin_not_allowed` 拦下（见 [api-contracts.md](api-contracts.md) §1.6）。

不要用 `command_execution_tool`、桌面自动化、HTTP 逐行发送或前端直连来绕过 workflow 的确认门和在线检查。

## 3. 完整发送的门禁

顺序与错误码以 `launch_send_job_for` 为准，任一失败都不写入加工指令：

0. **严格模式授权**（`MOSS_FUSION_STRICT=true` 时，技术书 §6.2；默认关闭，关闭时本步不读取任何数据、不改变判定）：唯一检查点 `require_r3_authority`（`crates/laser-api/src/approval.rs`）。桌面会话放行；小智桥接 / 局域网配对 / 网关只创建审批并返回 HTTP 202 `approval_required`，不连接设备；本机回环无凭据返回 403 `strict_mode_blocked`；以审批身份执行时（不论严格模式开关）按当前要素与审批保存的连接重算摘要并单次消费，执行使用审批保存的连接参数。无本地数据库时 503 `approval_unavailable`（失败即拒绝）。同一检查点也接在 `/api/safe-action` 的 R3 动作（`laser_contracts::safe_action_requires_approval`：除查询、急停、软复位、关激光、倍率复位与降低外一律视为 R3）与 `/api/device/framing`。停止类动作永不追加条件。
1. HTTP 层要求 `confirmed=true`，否则 400；`laser-mcp` 在转发前同样检查。
2. `authorize_send`：记录处于 `pending`；重新读取物理 G-code 文件计算 SHA-256 并与冻结值比对；存在有效 workflow 快照或 calibration SHA。改材料、厚度、文件、策略、参数或连接目标后必须重新预览。
3. 同一设备已有在途作业：409 `device_busy`。
4. 打开传输失败：`device_unreachable`。
5. 有界握手（3 秒）：发 `$$` 并消费到终止 `ok`，要求 `$32=1`，读取 `$30`、`$10`；失败 `device_not_ready`。握手无响应时提示可能正在跑 SD 脱机作业。
6. `$CD` 读取 FluidNC 配置；旧 GRBL 静默跳过。
7. 自适应预检：`$30` 必须为有限正数，非 1000 时按 `$30/1000` 在发送流水线中换算 S 值，原文件与 SHA 门禁不变；不写 `$30` / `$32`。发 `?` 要求 Idle，拒绝 Run/Jog/Home/Hold/Door/Sleep/Check/Alarm、SD 运行标记及 `Pn:` 含 `E`/`F`/`D`；失败 `device_preflight_failed`。
8. 设备预检后再读取物理 G-code，与确认快照的 SHA-256 重新比对；不一致返回 `send_gate_rejected`，不写入加工行。之后的流式发送和 SD 上传只使用这份已比对的内存内容。行校验剥注释、丢空行；超过 254 字节或含非 ASCII 拒绝整个文件（`gcode_line_invalid`）；空文件拒绝（`gcode_empty`）。
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
- 后端生成画板光栅时的工作区上限为 400×400 mm，不是翼宿的 100×100；行程限制以握手读到的 `machine_limits` 透传给前端约束画板。
- 经审批执行的发送在开始前把审批结果记为“发送中”（`phase=running`），发送线程结束时写回完成 / 失败 / 取消；进程在发送中崩溃后，启动恢复把这类审批记为失败、`error_code=result_unknown`，workflow 同时被记为 failed，任何一方都不会自动重发（A-33）。
- 内置语音（NV4）以 `CallerContext::Voice` 调用工具。运动与出光（R3）**跟随 `MOSS_FUSION_STRICT`**（2026-10-07 用户决定）：开启时只生成审批（来源 `voice`），关闭时与官方接入点相同，模型带 `confirmed=true` 即执行——此时唯一的确认是系统提示要求模型先复述参数、等用户口头确认，没有桌面批准。R4 与未分级工具（命令执行、打开应用、快捷键、打开网站）**不论开关都拒绝**（`HostBackend::always_deny_r4`），返回 `tool_not_allowed` 并记审计 `voice:<设备>`；工具清单与严格模式下的接入点相同（`laser_host::voice_tools_list`）。停止类动作照常立即执行。
- 工具开关（NV8，`mcp_disabled_tools`）只能减少可用工具：关闭的工具在接入点与内置语音的清单和调用两处都拒绝（`tool_disabled`，不触达后端）。`laser_safe_action_tool`（停止、急停）列在 `ALWAYS_ENABLED_TOOLS`，后端与语音中心都不允许关闭；新增停止类工具时须同时加入该列表。
- 小智接入点的工具分级在 `laser_contracts::tool_risk`（R0 只读、R1 生成、R2 降低风险、R3 运动或出光、R4 永不开放）。严格模式下接入点对 R4 与未分级工具返回 `tool_not_allowed`，R3 交给执行层生成审批；新增 MCP 工具时必须同时补分级，否则 `laser-mcp` 测试失败。
- 严格模式在桌面会话（P1-07 `X-Moss-Desktop-Session`）接入前开启，会让桌面端自己的发送也被当作本机回环拒绝；P1-07 完成前不要在实际使用中开启。
- `audit_log` 自 2026-10-06（P1-08）起由审批服务与接入点写入：审批创建 / 批准 / 拒绝 / 过期 / 执行 / 失败、模型自填 `confirmed` 被删除、R4 拒绝、严格模式拦截。创建、批准、执行三步与审计在同一事务中，审计写不进去就拒绝（R3 不放行，状态不变）；拒绝、过期、失败与停止类动作不依赖审计。
- `jobs`、`auth_pairings` 表已建但运行时无写入方；进度只在内存中的作业注册表与 workflow / calibration 记录的 `send_result` 里。

### 4.1 语音授权与停止修补（2026-10-10）

设备 token 校验后还需注册当前 device/client/session。撤销使所有旧会话标记永久失效，会话循环检查后关闭连接；重新允许只能建立新会话。工具执行入口再次检查，尚未消费的该设备审批同事务过期，预检或排队期间失效的语音发送不启动加工。已经执行的指令不因撤销自动撤回，桌面停止能力始终保留。

停止快捷通道只接受最终识别的完整短句，不因中间词“停止”抢先执行；仍在大模型前处理。无活动任务时需已明确配置的目标与现有 confirmed 契约，多任务未指定目标不随机选择；状态查询失败不等于任务失败，也不等于停机。界面持续重试并保留急停，请求写入成功只显示“已发送，停止未确认”。

本地判停在独立工作线程运行，事件循环不等待 IPC；队列和并发线程有界，排队到响应超时关闭异常会话，旧录音轮次的结果丢弃。不能宣称被阻塞的模型计算一定已被强制终止，也不能以软件通过代替实体急停或实机性能验收。

### 4.2 NV13 Codex Windows 沙箱权限负向测试（2026-10-11）

真实原版 `codex-cli 0.157.1` 在空白临时 `CODEX_HOME` 下，内置 `:read-only` 阻止测试工作区内/外写入，却**允许读取工作区外 canary 和连接本地 127.0.0.1 测试端口**；自定义仅授权 workspace 读取并禁网络的 profile 因缺 elevated Windows sandbox backend 无法启动。无硬件不等于本机隔离安全，绝不能把 `--sandbox read-only` 或成功 help 视为满足 NV13 的机密文件隔离。当前仍必须 fail-closed 禁止真实 AgentRun 子进程；未经另行批准不自动启用 Windows 提权沙箱、不改 OS ACL，不使用真实账号；见 NV13 实施文档 §5 测试表。

### 4.3 NV13 Job Object 不等于 Windows 读/网安全沙箱（2026-10-11）

纯 Windows 进程夹具已通过 Job 挂起创建、进程树、并发/超时/取消/撤销和限额测试；但 Job Object 不限制用户文件读取、网络或真实模型 CLI 使用的账号。Codex 0.157.1 普通只读沙箱仍可越界读和连接 loopback，严格权限 profile 缺 elevated backend。**无论 Job fixture 是否通过，真实 CLI 必须继续保持 `REAL_CLI_SANDBOX_VERIFIED=false`、桌面禁止启用**，不得绕过入口执行。文件/网络隔离与真实 CLI 实验需要另行授权。

### 4.4 NV13 Windows Home 的容器隔离备选（2026-10-11）

本机 Windows 家庭中文版未提供微软 Windows Sandbox，但已有可用 Docker Desktop Linux/WSL2 引擎及本地 `ubuntu:22.04` 镜像。新增 **测试专用** `scripts/test-nv13-container-isolation.ps1`，使用新建临时目录和 `--rm --pull never --network none --read-only --cap-drop ALL --security-opt no-new-privileges --pids-limit 8`，仅将新生成的工作区只读挂载；离线 canary/写入/宿主机回环连接/环境变量/cgroup/进程能力测试通过，未使用登录凭据或联网。Docker 容器隔离结果**不能推导为 Codex Linux CLI、Docker runtime 突破防御、模型 API 允许列表、MCP 或用户登录安全已通过**。不得直接向容器传入 Windows 用户主目录、Docker socket、MOSS secret 或无限制网络，也不得把容器实验用作解锁生产 AgentRun 的依据。以后需要明确的 Linux 版 CLI、独立认证、最小网络通道和完整审批/取消负向测试。
### 4.5 NV13 官方 Linux Codex 测试镜像（2026-10-11）

本轮已明确授权从官方 `openai/codex` 发布页下载 Linux CLI，在 Ubuntu 22.04 **专用测试 Docker 镜像**内安装；使用固定 SHA-256 校验发布压缩包、离线 Docker build，禁止额外镜像拉取或系统安装。测试只使用无凭据 canary 和 `--network none --read-only --cap-drop ALL --security-opt no-new-privileges --pids-limit 8`、非 root 用户、唯一只读工作区挂载；版本/help、离线文件/写入/网络隔离和容器退出通过，不等于真实 Codex 对话安全。

注意：裸 `docker run` **不会自动继承**上述运行时限制；只允许通过安全测试脚本/将来受审核的固定启动器启动，不得挂载 MOSS、Windows 用户目录、Docker socket 或认证文件，也不得开放未经受控的网络。Codex 自带的 Linux `sandbox` 依赖 bubblewrap；当前镜像不含 bwrap，所以内部沙箱启动失败。不能通过 `--privileged`、放宽 Seccomp 或无控制的网络访问绕过。生产真实 CLI/模型调用继续禁用，后续必须先验收独立认证、严格 API 出口代理、MCP 禁入及审批/撤销联动。
### 4.6 NV13 bwrap 与内部假模型代理边界（2026-10-11）

派生测试 Docker 镜像内 Ubuntu 官方签名的 `bubblewrap` 已成功安装，但 **当前 Docker 非 root + 默认 Seccomp + no-new-privileges 不允许 `unshare --user`**；真实 Codex Linux 内置 sandbox 因无法创建 user namespace 失败。严格禁止为了通过而加 `--privileged`、`CAP_SYS_ADMIN`、`seccomp=unconfined` 或放开 Windows 系统权限；内层沙箱当前状态必须是 BLOCKED，不是假 PASS。

另一个临时 Docker `--internal` 网络只连接一个返回固定人工 HTTP 数据的**假**代理，客户端没有默认外网路由也不能连本轮测试用 Windows 监听器，测试后网络和容器已清理。这只验证**容器内部可达**，不证明真实代理、TLS、远程 API 域名/IP 防绕过、模型密钥代理注入或审批撤销安全。真实 API 长期密钥绝不能进入容器、镜像、工作区或模型生成的脚本环境；如果将来使用一次性代理令牌，要把它视为容器可读取的最小权限短时令牌。见 `docs/integration/nv13-codex-docker-security.md`。真实 Codex 一直 fail-closed。
### 4.7 NV13 模型代理的离线策略测试（2026-10-11）

`laser-host::voice_agent::model_proxy` 仅实现固定假 HTTP `/v1/responses` 路由和一次性短 TTL 令牌校验，严格拒绝任意 URL、CONNECT、其他 HTTP 头、重复长度、非批准 JSON 模型及工具字段。仅 Rust 测试绑定临时 `127.0.0.1` 进行字节级回环，**不包含真实公网 HTTP/TLS 上游客户端，也不暴露生产服务端口**。使用 OS CSPRNG 签发的令牌在内存仅存摘要，绑定 run/device/实时许可、最多 30 秒/16 个租约；错误响应/审计不返回秘密。模型费用约束只有 synthetic `usage=0`，真实费用、鉴权、TLS、SSRF/DNS 及在途撤销完全未验收。`DisabledRunner` 与桌面 UI 的生产禁用不变。
### 4.8 NV13 真实 TCP + 本地假 TLS 与 Docker 固定启动方案（2026-10-11）

新增 `model_proxy/{transport,egress,tls_client}.rs` 与 `voice_agent/docker_plan.rs`，覆盖真实本机 socket 的有界 HTTP 长度/慢速/重复请求拒绝，固定 `api.openai.com/v1/responses` 的纯 IP/URL 规则，使用本机临时自签证书验收 TLS SNI 与不跳转、边界数据与撤销中的请求终止。安全性只证明**模拟 TLS 测试**；没有正式 TLS/真实 DNS 或凭据用量验收，不能让模型提供的任意 URL、CONNECTION/CONNECT 或 Docker 参数绕过限制。Docker 实际探针只加载 pin 到不可变 SHA-256 的官方 Codex 镜像进行 `--version`，仅挂合成临时目录、禁网、非 root、权限最小化；失败即停止且不运行模型。生产真实 CLI 始终使用 `DisabledRunner`，`REAL_CLI_SANDBOX_VERIFIED=false` 不得修改。
### 4.9 NV14 本地音乐选择、播放和撤销隔离（2026-10-11，软件完成 / 设备待验收）

音乐默认关闭且无目录，不自动扫描任何个人文件。只在用户显式启用并保存本地目录后，由桌面主窗口主进程验证 8 层、10,000 曲目/5,000 目录/50,000 项限额和本地安全根，拒绝 UNC、链接/Windows reparse、敏感系统根；受信索引生成 track_id，模型不能提供任意路径/URL 或用音乐混合其他工具。读取前按打开文件实际句柄、size/mtime 重新检查；复杂并发目录替换仍保留压力测试。音频 Symphonia 逐包转 24 kHz PCM，队列≤20×100ms、会话泵≤1秒；打断/新轮次/断线/撤销使用 generation、取消及发送前许可复核，歌曲不进入 LLM 历史。新增 voice_music_status/save/pick_directory 只给桌面 main 窗口，选择文件夹只在显式点击后弹框且不自动扫描。纯软件假小智 Opus、abort、revoke 通过，真实 MP3/FLAC/Ogg 文件、实体音质/长曲仍未验收。生产 Codex 权限门禁独立保持 disabled。
### 4.10 NV15 Home Assistant 控制门禁软件基线（2026-10-11）

新增 `laser-host::voice_home` 仅做只读 HA 状态/服务 schema 校验与未执行的固定服务调用计划。`mock_http` 只能在 Rust `cfg(test)` 下访问程序自己创建的 `127.0.0.1` 临时服务，全部请求是合成 `GET /api/states` 与 `GET /api/services`；真实 HA Endpoint/DNS/TLS/token 或 `POST /api/services` **均没有生产连接**。未经用户逐实体选择、当前服务不存在、状态 unknown/unavailable、修改了模型输入而增加属性、危险高风险 allow 策略一律拒绝。对外可用状态仅允许最少的温度/亮度/单位数字；任意 PIN、密码、token 及任意嵌套属性全部丢弃。

真实 HomeControl 必须将 endpoint、凭据 revision、实体策略 revision、设备许可、动作、目标、参数摘要绑定到**专用持久事务审批**，批准与真正发送前重新校验；要求审计成功才可操作。高风险域始终 Ask。模拟批准令牌只有内存单次消费测试，不等于正式批准。当前 `execute_without_home_control_approval` 总是拒绝，未注册生产工具，也不使用 laser ExternalTool 或 `confirmed=true`。任何实际家居动作都需后续用户逐实体明确授权与实测。

### 4.11 NV15–NV17 新增模拟安全门禁（2026-10-11）

- **NV15 Home Assistant**：测试专用模拟控制在批准和实时许可重校验后**至多一次 POST**；收到与否不明确、HTTP 错误或许可撤销一律 `result_unknown`，绝不自动重发；最多三个只读目标状态 GET（总时限3秒）且只允许授权实体。专用 SQLite HomeControl 终态不可被迟到成功覆盖。真实 HA 连接、凭据和任何家居控制入口仍禁用。
- **NV16 声纹**：纯合成 embedding 只验证固定元信息、相似度、模型/generation 隔离和并发上限；未校准时返回 Unknown，`may_authorize_action` 恒 false。声纹永远不能替代加工桌面审批/急停；无真人采集、实际声纹模型、按人记忆或可信身份门禁。
- **NV17 固件**：仅受信传入假文件、16MiB/件与256MiB总体限额、sha256、默认关闭、revision 确认和设备当前版本合法性演练；无真实固件、持久下载凭据、API 路由，OTA 继续 `firmware:None`，严禁据模型自报 device-id 推送或下载。实际刷写/断电恢复须额外设备授权和恢复方案。

### 4.12 NV18 语音 sidecar 构建资源安全（2026-10-11）

`build-windows-installer.ps1` 入口已转到独立 `build-nv18-sidecar-safe.ps1`；旧逻辑保留审查但不会再递归删除现有未跟踪资源。按次只生成新 GUID 资源目录，禁止覆盖用户已有 `bundle-resources/voice-local`。对应 helper/源码/COPYING/初步 notices 已构建进入 NSIS；`verify-nv18-nsis-in-docker.ps1` 以禁网只读非 root Docker 列出真实安装包并验证上述条目，清理自身检查镜像。**还未满足 sherpa-onnx/espeak-ng 及静态依赖的完整许可证/对应源码审计；不得因此对外分发**。安装/升级/卸载、数据迁移和真实 helper 启动均需明确授权的安全环境验收；构建过程不得运行安装器或连接真实硬件。
### 4.13 语音中心插件/私人记忆的 Tauri ACL 本地页面隔离（2026-10-11）

桌面主窗口继续导航到本机 `http://127.0.0.1:<端口>/`，该页面对 Tauri 来说属于 remote origin，**即使 window label 是 `main` 也不能授予插件密钥状态及私人记忆权限**。新 `voice-private` 窗口只能加载安装包资源 `voice-private.html`，在 Tauri capability 里不配置 `remote`，仅该窗口能调用 `voice_plugins_{status,save}`、`voice_memory_{list,set_enabled,save,clear}`。Rust 另查 webview 标签+app 资源 URL，拒绝其他 host、路径、端口或 query；主窗口仅可要求打开子窗口，不读取返回敏感数据。不得将这些权限加入 default `windows:["*"]` 或允许 `127.0.0.1:*` 的 remote 能力，也不得转交普通浏览器或小智设备。详见 `docs/integration/voice-private-acl.md`。安装后真实 Windows WebView2 的授权链仍需实际页面验证。

### 4.14 NV15 家居控制读回时撤销仍禁止确认成功（2026-10-11）

HomeControl 专用持久审批在消费后至多提交一次。设备许可若在目标状态 GET 过程中撤销，**即使旧 GET 返回匹配状态，也不得写入 Completed**，必须持久记录 `result_unknown` 且不重发。每次目标 GET 接受当前剩余 3 秒总预算，独立完整 HTTPS 请求超时上限 900ms。本机合成 TLS 测试仅使用临时回环、自签 CA 和假令牌检验网络与目标身份；未接入真实 Home Assistant，不能由软件模拟测试解锁家居设备动作。

### NV15 令牌备份、撤销竞态与审批闭环（2026-10-11）

迁移时只允许从 `voice-private` 桌面窗口导出/导入固定受限 `moss-home-profile-v1` 白名单；不能打包 SQLite/WAL/SHM 或迁移原始 DPAPI 密文。恢复必须二次确认，并且先撤销本机旧 HA token、voice read/control grants 和待审批 HomeControl，再将所有实体置为禁用；重新输入访问令牌且再次审批后才可运行。敏感 token/config 改动与家居唯一 POST 发送边界串行化，双连接 CAS 用 `BEGIN IMMEDIATE` 消除旧快照提交；家居控制最终状态与审计同事务入库。**已发送的外部控制无法撤销；旧 WAL/快照可能仍包含历史明文，不能自动删除，疑似泄漏需去 HA 服务端轮换/撤销 token。** 所有修复仅基于合成凭据、离线 SQLite 与假网络夹具验收，真实家居、桌面确认及固件/激光实机留最终单独授权。
**追加约束**：可移植 HA profile 只能包含不带 origin、媒体库 ID、许可或已启用标志的实体清单。遇到来自其他 Windows 账户的 DPAPI 密文时，数据库允许受限启动以提供明确确认的撤销迁移；任何读取凭据/HA 服务请求/控制批准都必须失败。该流程以单调版本号原子清除旧密文，而非解密或迁移长期令牌。家居每条申请都有独立 active 撤销标志，取消和凭据变更与唯一 POST 提交前的检查使用同一 send_fence；模拟服务接受但结果不明或提交后撤销，一律持久记 Unknown、不可重试，不能用迟到响应改写 Completed。

### 4.15 NV15-1 历史备份审计安全窗口（2026-10-11）

用户须在本地 `voice-private` 安全页面明确按按钮，由 Rust 原生文件选择器选择单个历史文件。命令不接受前端提供的路径；限制绝对本地路径、非 `..`、普通非链接文件及 64MiB，只读分块检查标记。Windows 任何 reparse 路径组件及网络路径拒绝。输出仅固定类别/字节数/禁止分享标志，不包含路径、文件内容、凭据、JSON 原文或 DPAPI 数据。未知文件默认敏感，不能作为已脱敏证明。仅处理用户主动选择的单文件，不自动扫描历史备份、WAL、私人数据，不自动恢复或删除。历史疑似泄漏需用户自行在 Home Assistant 服务端撤销、轮换旧 Token；真实文件与 WebView2 操作留 FINAL。

日志与诊断回读额外注意：`moss_voice::redact::redact_line` 对字段名 token/access_token/api_key/authorization 等和裸 Bearer/Basic 凭据执行遮盖，`RotatingLog::tail` 重脱敏旧日志，不会原地编辑用户已有日志。该机制不是任意数据/任意令牌格式的完整 DLP；不得将旧日志、SQLite/WAL、原始诊断转储或备份判定为可安全分享。

### NV15 正式控制禁用与统一网络出口（2026-10-11）

`HomeControlCoordinator::approve` 在验证尚未通过时始终返回 `home_control_disabled`，不消费审批、不创建发送用客户端；`HttpsHomeTransport::submit_once` 在非测试编译下也直接拒绝，不发真实 POST。测试使用 mock/fake 合成传输。只读 GET 与潜在控制 HTTPS 传输共享 `moss_voice::home_readonly::pinned_client` 的 DNS/IP 私网地址检查、一次性固定解析、HTTPS 校验、无代理及不跟随重定向规则；不意味着已允许正式 POST。正式授权、网络竞态与实际 HA 的证据不满足前不能打开门禁。

NV14 音乐已增加敏感子目录跳过，重名候选仅显示非路径索引 ID 与编号，不明确选择就不播放。S1-09 Android 实际来源必须经安装设备核验，不得用 APK 构建通过冒充实机来源验收。

### 4.16 主窗口特权命令固定到本进程后端地址（2026-10-13）

主窗口导航到 `http://127.0.0.1:<端口>/` 后是 remote 来源，capability 的 `remote` 只能写 `http://127.0.0.1:*`，静态配置挡不住其他本机端口的页面。凡能取得桌面会话、改写配置或密钥、启停进程的命令（`get_desktop_session`、语音中心与内置语音的改写类命令、视觉/音乐/Agent 命令、第三方 MCP 管理），必须在函数开头用 `window_is_trusted_main` 校验 `main` 标签与精确来源，失败即拒绝；新增此类命令照此处理，并加入 `main_window_commands_pin_backend_origin` 的清单。只给主窗口用的 capability 必须声明回环 `remote`，否则 Tauri 只对本地打包页授权、主窗口调用全部被拒。私密数据仍只走 §4.13 的 `voice-private` 窗口。

### 4.17 家居控制与声纹识别改为用户开启（2026-10-14 用户决定）

本节取代 §4.10、§4.11 中“生产硬关闭 / 未校准恒为 Unknown”的表述，其余约束不变。

- **家居控制**：删除源码常量 `PRODUCTION_CONTROL_READY`，`HttpsHomeTransport::submit_once` 在生产中真实发送。开启条件全部由用户在私密窗口完成：HTTPS 地址、令牌、勾选白名单实体、明确同意语音控制（GRANT 绑定当前白名单与凭据版本，改任一项即失效）。语音只能生成审批；每个动作都要在私密窗口“家居控制审批”里批准，全局审批卡只提醒并打开私密窗口（主窗口的通用批准接口按设计拒绝 HomeControl）。至多一次 POST，结果未知不重发，批准前重新读取并核对状态与白名单。未用真实 Home Assistant 验收，界面提示先用无害设备试用。
- **声纹识别**：删除写死的 `calibration: None`。用户登记至少两人后，每人另录三段测试音，`person_context::calibrate` 以“认成别人”的最高分加 0.05 为阈值（下限 0.5、上限 0.95），自检要求测试集内零误认、本人认对不少于三分之二；结果存于 `app_secrets`（`voice.speaker.calibration`），只对标定时的登记名单（编号与版本）有效，名单变化即失效。只拿已登记的人互比，测不到陌生人；这是候选身份提示，**永远不能授权加工、家居或任何设备动作**，按人记忆仍需另行明确开启。
- Agent 执行（§4.2～4.8）与固件下发（§4.11）不变：仍硬关闭。

## 5. 测试要求

测试隔离、`FakeGrbl` 字节级校验、SHA-256 重算、`0x18` 优先级和结果状态见 [`AGENTS.md`](../../AGENTS.md)。

## 6. 文档和提示词同步

- 改工具名、参数、注册开关、默认连接方式或确认规则时，同步 [`docs/integration/xiaozhi-prompts/latest.md`](../integration/xiaozhi-prompts/latest.md) 并新增日期快照；控制台正文 ≤2000、知识库 ≤200 字符（空格与换行计入）。
- 改路由、字段、错误码时同步 [Web API 契约](api-contracts.md)；改表结构时同步 [数据库结构](db-schema.md)；功能可见变化同步 [功能字典](feature-glossary.md)。
