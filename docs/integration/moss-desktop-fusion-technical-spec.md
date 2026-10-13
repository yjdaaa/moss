# MOSS 自建小智语音链路与多项目融合技术书

状态：技术书 r13（2026-10-10）。本文是拟实施方案，不代表当前能力：“现有”指已在代码中核实的行为，“拟新增”指未实现的契约。进度只看 [实施 TODO](moss-fusion-implementation-todo.md)；内置语音设计见 [内置语音技术书](moss-native-voice-spec.md)，r11 起它取代本文部分非目标与决定。外部服务端链路实施前须完成 §10 的 P0 前置验证。

修订记录：

| 版本 | 变更 |
| --- | --- |
| r1 | 初稿 |
| r2 | 可实施性审查后修订：①新增调用方上下文 `CallerContext`（D10、§6.4），严格模式改为依赖它而非回环判断；②补充现有 CORS `*` + 回环免鉴权的跨站触发风险及收紧方案（D11、§6.5）；③更正回滚结论——旧版程序可直接打开 v4 库（§9.3）；④P2 拆分，先走 MCP 接入点零代码验证 R0/R1（D2、§10）；⑤审批绑定显式解析后的连接目标并保存连接参数（§5.4、§7.1）；⑥“停”只映射 `cancel` / `emergency_stop`（§6.1）；⑦按上游 README 更新上游事实 |
| r3 | 用户决定：①自有模型为**云端部署**，同时提供 OpenAI 与 Anthropic 协议（地址 + 密钥），P0-1 由本机硬件评估改为端点能力与延迟验证，xiaozhi-server 侧统一走 OpenAI 协议（§2、D7、§9.1、§10）；②**不允许任意来源访问**：CORS 收紧与来源校验改为默认始终生效，不再依赖严格模式，并拆为可先行交付的独立工作包 S1（D11、§6.5、§10） |
| r4 | 用户采纳“一个入口，后台多进程”：新增 MOSS **语音中心**（D12、§9.4）——链路模式单选（官方 / 自建，互斥）、xiaozhi-server 由 MOSS 启停（只管自己启动的进程）、模型配置向导（密钥写入服务端配置，MOSS 不保存）、状态与日志；D7 进程管理规则相应放宽；P4 改为语音中心工作包；新增验收 A-60..A-69 |
| r5 | 通读上游源码（xiaozhi-esp32-server 提交 `87c6df7`，2026-09-29）后修订：①**正式链路改为 MCP 接入点**（D2）——上游 xiaozhi-server 与 MOSS `run_bridge` 都是接入点的 WebSocket 客户端，由独立的 `mcp-endpoint-server` 中继，MOSS 无需写插件；严格模式通过 `CallerContext::McpBridge` 生效；网关 + 插件降为可选增强 P2b（只为审批结果语音播报与会话级审计）；②三个模型独立配置，按上游真实字段写入 `data/.config.yaml`（§9.4.4）；③设备在线状态改用接入点 `health` 接口的 `robot_connections`；④补充 30 秒工具超时、ASR 非流式等约束 |
| r6 | 新增桌面端前端改造（§9.5）：语音中心页、全局审批卡、顶栏真实状态（修复 `/api/system/info` 编造的 MCP 遥测，独立工作包 S2）、`apiFetch` 携带桌面会话头、整体视觉提亮（UI-1）；原型 `frontend/ui-preview-voice-center.html`；新增验收 A-70..A-79 |
| r7 | （同日补充：背景选定 WebGL「等离子流体」并已先行接入现有前端 `CyberBackground.jsx`，构建与实机待补验证；其余三种仅作原型备选）视觉提亮 UI-1 升级为**前端整体改版 UI-2**（§9.5.5）：曜石设计 + 流浪地球太空舱背景，原有按钮功能全部保留并附功能对照清单；原型 `frontend/ui-preview-forge-v2.html`。设计仍为草案，用户后续会继续修改 |
| r8 | 新增 §9.6 安装与卸载：Windows NSIS 安装包（自定义安装目录、当前用户/所有用户、快捷方式、覆盖升级）、卸载（默认保留数据，可选删除 `%APPDATA%\moss`）、WebView2、签名、Android APK；详细规范独立成文 `docs/developer/packaging-install.md`。核实并修正旧文档：运行目录在 Windows 上实际是 `%APPDATA%\moss`，不是程序目录旁 `.runtime/`；`.env` 依赖启动目录，安装版需改用界面设置。新增工作包 PKG 与验收 PK-01..PK-11 |
| r9 | 新增 §9.7 导出目录设置：桌面端可在偏好设置中选择导出文件的保存目录（系统文件夹选择框、恢复默认、打开文件夹、可选“每次导出时询问”）；核实现状为 `save_export_file` 固定写入“下载”文件夹（无则退回“文档”或应用数据目录 `exports`）；目录只能经桌面壳命令修改，`/api/settings` 不接受该字段；目录不可用时报错并让用户选择，不静默改写到别处；Android 与浏览器保持现状。新增工作包 EXP 与验收 A-82..A-88 |
| r10 | 新增 §9.8 手机连接电脑 MOSS：参照 OctoPrint / Klipper 的“主机为唯一数据源”，手机 App 增加“连接电脑”模式（请求发往电脑，不做双向同步）；电脑偏好设置“手机连接”开关（`settings.json` `lan_access` → 监听 `0.0.0.0`）与配对码；局域网调用方对材料、校准参数与系统设置只读（`lan_read_only`），雕刻机操作不受影响。新增工作包 LINK 与验收 A-89..A-96 |
| r11 | 用户决定（2026-10-07）：把 xiaozhi-esp32-server、海门、xiaozhi-client 的功能做进 MOSS 本身（“内置语音服务”），取各项目最优实现，本地与云端模型都支持；范围含第三方 MCP、记忆、视觉、插件（天气、新闻、农历、搜索、RAGFlow、音乐、Home Assistant）、Claude Code / Codex 调用、声纹、固件下发；设备端口 8000 / 8003；模型与插件密钥存 SQLite `app_secrets`；外部服务端链路保留作兼容。方案见 [MOSS 内置语音服务技术书](moss-native-voice-spec.md)（n3）。本文 §1.3 第一条非目标取消；D1、D7 中“由 xiaozhi-esp32-server 编排 / 只由 xiaozhi-server 调用模型”改为“内置服务或外部服务端二选一”；§9.4.4“MOSS 不保存密钥”与 A-67 只适用于外部服务端链路，内置链路的规则见该书 §6；其余安全决定（D3、D4、D8、D10、D11）不变 |
| r12 | 文档同步（2026-10-10）：当前版本引用统一为融合 r12 / 内置语音 n18 / 实施细则 i1；新增 NV10～NV18 可执行实施入口；§9.5.5 落地方式改为已选定 WebGL 等离子流体，Canvas 星点仅作叠加；NV18 helper 打包的配置接线与源码/许可/安装验收分别记录。保留历史版本与既有实机未验项，不把文档更新视为功能完成 |
| r13 | 剩余技术实施说明（2026-10-10）：修正内置语音严格模式交叉引用、P4 对可选 P2b 的错误硬前置；新增 P2b/P3/P5 六项实施细则 s1（模块/步骤/接口/异常/验证），保留 P2b/P5 的可选地位；UI-2 旧太空舱表述对齐已选 WebGL 等离子流体。NV18 helper 已有接线与待完成交付条件不冒充实际安装通过 |
范围：Windows 桌面宿主（`moss-desktop`）+ 内置语音服务或外部自建小智服务端（xiaozhi-esp32-server）+ 自有云端模型（ASR/LLM/TTS，OpenAI/Anthropic 协议）+ 可选的海门 Agent 与 xiaozhi-client。Android 保持现有行为，只做兼容回归。
依据：[架构总览](../developer/architecture.md)、[开发者安全入口](../developer/safety.md)、[Web API 契约](../developer/api-contracts.md)、[数据库结构](../developer/db-schema.md)、[小智 GRBL 工具边界](../laser/xiaozhi-grbl-tool-boundary.md)、仓库根 `AGENTS.md`。与代码不一致时以代码为准并回修本文。

---

## 1. 背景、目标与非目标

### 1.1 背景

- 现有链路：小智设备（立创·实战派 ESP32-S3）→ 小智官方服务器（ASR/LLM/TTS 与 MCP 接入点）→ MOSS `laser-mcp` 出站 WebSocket 桥 → `dispatch_tool_call_with` → `HostBackend` → 进程内 `ApiService`。此链路已与 MOSS 跑通。
- 激光机下位机为 FluidNC，引脚配置已由用户实机测试可用；MOSS 侧门禁见 [安全入口](../developer/safety.md) §3。
- 痛点：官方服务器的模型、提示词长度（控制台 ≤2000、知识库 ≤200 字符）、工具调用策略与会话控制不可定制，部分功能无法实现。
- 目标方向：用自建服务端替换官方服务器，ASR、LLM、TTS 全部换成自有模型（接受更慢的响应，换取可玩性与功能自由度）。
- 模型部署（用户确认）：自有模型部署在云端，同时提供 OpenAI 协议与 Anthropic 协议接口，以“地址 + 密钥”方式接入；不在本机运行推理，因此不评估本机硬件。

### 1.2 目标

| 编号 | 目标 | 衡量方式 |
| --- | --- | --- |
| G1 | 小智设备连接自建服务端，完成唤醒 → ASR → LLM → TTS 的完整对话 | §11 A-01..A-04 |
| G2 | 自建服务端的 LLM 能调用 MOSS 全部只读与生成类工具，结果与官方链路一致 | §11 A-10..A-13 |
| G3 | 任何会让机器运动或出光的动作，必须由桌面端人工批准，**模型不能自行确认** | §11 A-20..A-27 |
| G4 | 任一外部组件（模型、服务端、Agent）宕机不影响 MOSS 自身的网页/桌面加工与急停 | §11 A-30..A-34 |
| G5 | 可随时回退到官方小智链路，且不丢用户数据 | §11 A-40..A-42 |

### 1.3 非目标

- ~~不重写 xiaozhi-esp32-server、海门或 xiaozhi-client 的源码；首版只做配置与最小插件。~~（r11 取消：改为内置语音服务，见 [MOSS 内置语音服务技术书](moss-native-voice-spec.md)）
- 不把多个项目合并为一个仓库或一个进程（先产品/服务融合，后按收益合并源码）。
- 不实现 exactly-once 物理执行；不承诺完整审计恢复（见 §8）。
- 不在 Android 上托管模型或服务端；Android 远程控制自建链路属于独立范围。
- 不修改 FluidNC 固件，不改 `self.grbl.*` 固件侧工具。
- 不为文档任务更新 `CHANGELOG.md`（实施阶段的代码改动另按 `AGENTS.md` 记录）。

---

## 2. 现状基线（已核实）

| 领域 | 现状 | 对本方案的含义 |
| --- | --- | --- |
| MCP 服务 | `laser-mcp` 实现 JSON-RPC 2.0：`initialize`/`ping`/`tools/list`/`tools/call`；`tools/list` 来自编译期黄金清单 `tests/golden/mcp_tools.json`；20 个工具（未开开发者开关时 19 个） | 工具面可直接复用，不需要新工具注册 |
| MCP 传输 | 只有 `run_bridge` 出站 WebSocket 客户端，地址取 `settings.json` 的 `mcp_endpoint` 或 `MCP_ENDPOINT` | 自建服务端若提供兼容 MCP 接入点，理论上改地址即可；**兼容性未验证** |
| 宿主转发 | `HostBackend::request(method, route, body)` 由 `laser-host::InProcessBackend` 实现，转发到进程内 `ApiService` | 新网关可在进程内复用同一套门禁 |
| 确认语义 | `laser_workflow_tool confirm_send` 由**调用方**传 `confirmed=true`；`laser-mcp` 与 HTTP 层只检查该字段存在 | **模型可以自己填 `confirmed=true`**，这是融合必须收紧的缺口（D3） |
| HTTP 服务 | 同步实现，回环免鉴权；`/v1/events` 只发一次 `ready` | 回环免鉴权意味着同机运行的服务端/Agent 也被视为可信，严格模式必须取消该假设（D4） |
| 局域网鉴权 | `AuthManager` 配对 token 只在内存；`auth_pairings` 表无写入 | 设备身份与桌面身份需分开设计（D6） |
| 存储 | SQLite schema v3；`audit_log`、`jobs` 表已建但无运行时写入方 | 新增审批表需升 v4；审计需新增写入方 |
| 打包 | `tauri.conf.json` 中 `bundle.active=false`，无 `externalBin` | 安装包与 Sidecar 均不是既有能力，属于 P4 新工作 |
| 发送门禁 | `launch_send_job_for`：SHA 冻结 → 设备忙 → 握手 → `$CD` → 预检 → SHA 复核 → 流式发送 → `G4 P0` + Idle；崩溃/断线标“结果未知”，不自动重发 | 全部保留；审批只是在其**前面**再加一道 |
| 调用方身份 | `ApiService::serve` 只按 `peer.ip().is_loopback()` 区分来源；`handle_http_from(request, loopback, source_ip)` 之后的 `handle(method, path, body)`、`launch_send_job_for(kind, workflow_id, gcode_path, expected_sha, object)`、`handle_safe_action(args)` 均**不带调用方身份**。`InProcessBackend::request` 拼装 HTTP 报文后调用 `handle_http`（固定 `loopback=true`），因此 MCP 调用与桌面 UI 在后端不可区分 | 执行层严格模式必须先引入调用方上下文（D10）；不能放进请求体（可伪造） |
| CORS | 所有 JSON/OPTIONS 响应带 `Access-Control-Allow-Origin: *`，`Allow-Headers` 为 `Content-Type, Authorization, Accept` | 结合回环免鉴权，本机浏览器中任意网页可向回环端口发起 `confirm_send`（现有风险）；自定义头需加入白名单（D11） |
| 连接目标 | `connection_spec_from_request` 在串口留空时自动选择首个枚举到的系统串口 | 审批摘要必须基于解析后的目标，且 R3 审批要求显式目标 |
| 加工中安全动作 | `handle_safe_action` 在有在途作业时只放行实时字节（急停/软复位 `0x18`、倍率）与 `status`，经在途作业的同一传输写入；`laser_off`、移动等顺序指令被拒绝 | 语音“停”只能映射 `cancel` / `emergency_stop`；急停不与 G-code 排队 |
| schema 前向兼容 | `RuntimeDb` 迁移只执行 `current < N` 的步骤，遇到更高版本不拒绝 | 旧版程序可打开 v4 库并忽略新表，回滚无需降级脚本 |

上游项目（r5 起已读源码，均未运行）：

| 项目 | 角色 | 已知 | 未验证 |
| --- | --- | --- | --- |
| xiaozhi-esp32-server（xinnan-tech） | 自建小智服务端：设备 WebSocket、VAD/ASR/LLM/TTS 编排、插件函数、MCP | MIT 许可证；两种部署：最简化（配置文件，单智能体；含 FunASR 建议 2 核 4 GB）与全模块（数据库 + 智控台，多智能体；含 FunASR 建议 4 核 8 GB），均支持 Docker 与源码；支持设备端 MCP、**MCP 接入点协议**、服务端 MCP 三种方式；插件目录 `plugins_func`，支持热加载；本地模型选项：ASR FunASR/SherpaASR，LLM Ollama/Xinference 等（OpenAI 兼容），TTS FishSpeech/GPT-SoVITS/Index-TTS/PaddleSpeech；设备经 OTA 接口地址获取 WebSocket 地址。`config.yaml` 中：LLM 有 `type: openai`（`base_url`/`url` + `api_key`），**没有 Anthropic 类型**；ASR 有 `type: openai`（`OpenaiASR`，调用 `/audio/transcriptions`）；TTS 有 `type: openai`（`OpenAITTS`，`api_url` 指向 `/audio/speech`，另需 `model`、`voice`）；工具调用需 `Intent: function_call` | r5 已读源码核实（提交 `87c6df7`）：见下方“上游源码核实”表；仍未实测运行 |
| xiaozhi-client（shenjingnan） | MCP 聚合与工具可见性管理 | 可聚合多个 MCP 服务 | 能否作为自建服务端的工具上游；管理 API |
| 海门（本机 `haimen`） | Agent 注册与语音提供商工厂 | 存在 `xiaozhi_asr_llm_tts`、`tts_factory`、`agents/registry` | 不是通用本地模型服务；可导出 API 未确认，本轮未重跑 |
| Tauri Sidecar | `externalBin` + 目标三元组命名 | 官方文档 | 不解决模型环境、GPU 依赖和崩溃恢复 |

上游源码核实（xiaozhi-esp32-server `main/xiaozhi-server`，提交 `87c6df7`，MIT）：

| 主题 | 源码事实 | 对本方案的影响 |
| --- | --- | --- |
| 启动 | 源码部署为 Python 3.10，在 `main/xiaozhi-server` 下运行 `python app.py`；必须存在用户配置 `data/.config.yaml` | 语音中心启动命令固定为该形式 |
| 配置合并 | `config/config_loader.py`：读取 `config.yaml` 后与 `data/.config.yaml` 合并；若 `data/.config.yaml` 含 `manager-api.url`，则改为从智控台拉配置 | 向导只写 `data/.config.yaml`；检测到 `manager-api.url` 时只测试不写 |
| 端口 | `server.port` 8000（设备 WebSocket `/xiaozhi/v1/`）；`server.http_port` 8003（OTA `/xiaozhi/ota/`、视觉接口） | 与 MOSS 默认 9666 不冲突 |
| LLM | `type: openai`，字段 `url`（基础地址）、`api_key`、`model_name` | 向导字段映射见 §9.4.4 |
| ASR | `OpenaiASR`：`type: openai`，字段 `base_url`（**完整** `/audio/transcriptions` 地址）、`api_key`、`model_name`、`output_dir`；实现为整段音频一次 `requests.post` 上传，非流式 | 识别延迟随句长增加 |
| TTS | `OpenAITTS`：`type: openai`，字段 `api_url`（**完整** `/audio/speech` 地址）、`api_key`、`model`、`voice`、`speed`、`output_dir` | 同上 |
| Anthropic | 运行代码中没有 Anthropic provider | 服务端统一走 OpenAI 协议（D7） |
| 工具调用 | `selected_module.Intent: function_call`；插件在 `Intent.function_call.functions` 中按模块名启用 | P2b 插件需加入该列表 |
| MCP 接入点 | `core/providers/tools/mcp_endpoint`：xiaozhi-server 以 **WebSocket 客户端**连接 `mcp_endpoint` 地址，发送 `initialize`、`tools/list`，调用 `tools/call` 并等待结果，**默认超时 30 秒**；接入点本身是独立项目 `xinnan-tech/mcp-endpoint-server`（默认端口 8004），工具方（如官方示例 `mcp_pipe.py`）用同一个 `ws://…/mcp_endpoint/mcp/?token=…` 地址作为客户端接入 | MOSS 的 `run_bridge` 与 `mcp_pipe.py` 角色相同，**无需改代码即可接入**；单次工具调用须在 30 秒内返回 |
| 接入点健康 | `mcp-endpoint-server` 提供 `/mcp_endpoint/health?key=…`，返回 `tool_connections`、`robot_connections` | 语音中心可据 `robot_connections > 0` 判断有设备会话在线 |
| 插件 | `plugins_func/register.py`：`@register_function(name, desc, ToolType)`，返回 `ActionResponse(Action.REQLLM \| RESPONSE \| …)`；`SYSTEM_CTL` 类型会传入 `conn`，支持 `async def`；`core/handle/intentHandler.speak_txt(conn, text)` 可主动播报 | P2b 可实现“批准结果语音播报”，但依赖上游内部函数，需锁版本 |
| 设备状态接口 | 单模块 HTTP 服务只有 OTA 与视觉路由，没有设备在线查询接口 | 改用接入点 `health`（上一行） |


---

## 3. 关键设计决定

| 编号 | 决定 | 理由 | 放弃的方案 |
| --- | --- | --- | --- |
| D1 | **单一语音编排者**：一个会话只由 xiaozhi-esp32-server 编排 ASR→LLM→TTS；海门 Agent、xiaozhi-client 只作为可选工具提供方 | 避免两个引擎同时响应同一句话、重复执行工具、TTS 抢播 | 海门与服务端并行监听设备 |
| D2 | **正式链路走 MCP 接入点（r5）**：自建 xiaozhi-server 与 MOSS `run_bridge` 都连接自部署的 `mcp-endpoint-server`，MOSS 不写插件、不开网关；P1 的严格模式经 `CallerContext::McpBridge` 对接入点调用同样生效。P2a 先只验证 R0/R1，P1 完成后同一条链路即可承载经桌面审批的加工。**网关 + 插件降为可选 P2b**，只在需要“批准后语音播报结果”或会话级审计/幂等时再做 | 源码证实接入点是现成的双客户端中继，MOSS 现有桥即可接入；省掉网关、令牌、幂等表与插件维护，P1 范围明显缩小 | 首版就做插件与网关（r1–r4 方案）；为服务端 MCP 新增 stdio/SSE 服务器 |
| D3 | **模型无权确认加工**：所有运动/出光类动作在融合模式下只产生“待批准请求”，由桌面端人工点击批准；批准绑定动作摘要、设备目标、一次性授权与幂等键 | LLM 会幻觉“用户已确认”；语音 ASR 也可能误识别“确认” | 语音口令确认；模型填 `confirmed=true` |
| D4 | **严格模式在业务执行层拦截**：开启后，`launch_send_job_for`、安全动作、走边框、材料矩阵发送等入口统一检查审批授权或桌面会话凭据，旧 HTTP/MCP/别名入口同样受约束 | 只保护新网关等于留后门（旧 MCP 桥、`/api/text-task/send` 别名、回环 HTTP） | 只在网关层检查 |
| D5 | **不承诺 exactly-once**：崩溃窗口内的作业标记“结果未知”，只查询不重放 | 与现有 `recover_after_restart` 语义一致；物理机器状态无法从软件侧证明 | 自动重试 |
| D6 | **新接口默认关闭并鉴权**；网关令牌、桌面会话凭据、局域网配对 token 三种身份相互独立 | 回环免鉴权在同机多进程场景下不再成立 | 复用局域网配对 token |
| D7 | **模型为云端服务，只由 xiaozhi-server 调用**：ASR/LLM/TTS 均以“地址 + 密钥”配置在 xiaozhi-server（统一用 OpenAI 协议的 `type: openai`，因上游无 Anthropic 类型；Anthropic 协议留作 P5 自研 Agent 备用）；MOSS 不保存模型密钥、不托管模型；**例外（r4）**：语音中心的配置向导可在用户点击“测试连接”时，用内存中的密钥对端点发起一次测试请求，结束即丢弃（§9.4.4） | 模型已云端部署；减少密钥扩散面；与上游现有 provider 直接对接，无需写适配代码 | 在 MOSS 中代理模型请求；为 Anthropic 协议写上游 provider |
| D8 | **MOSS 是加工状态与授权的唯一管理方** | 复用现有 workflow、`HostBackend` 与设备门禁 | 在服务端插件中维护加工状态 |
| D12 | **一个入口，后台多进程（语音中心）**：用户只打开 MOSS；MOSS 的“语音中心”页面负责链路模式单选、启停 xiaozhi-server、模型配置向导、状态与日志、待审批入口。xiaozhi-server 仍是独立 Python 进程，不并入 Rust 应用；MOSS 只启停**自己启动的**服务端进程，外部启动的只显示状态 | 日常只开一个应用；保持各程序独立，可单独升级与回退；单选从界面上保证 D1 | 每个项目一个功能按钮并允许同时开启；用 Rust 重写服务端；首版就把 Python 环境打进安装包 |
| D9 | Windows 完整宿主；Android 保持现状，Windows-only 组件用 `cfg(windows)`/feature 编译隔离 | 不把伴生进程方案强搬到手机 | 双端同时支持 |
| D10 | **引入调用方上下文 `CallerContext`**，从 HTTP 入口与 `InProcessBackend` 一路传到所有执行入口；严格模式依据它判断，不再依据回环 | 现状下 MCP 调用与桌面 UI 在后端不可区分（§2）；身份放进请求体可被伪造 | 在请求体加身份字段；只在网关层判断 |
| D11 | **不允许任意来源访问（用户决定，默认始终生效）**：去掉 `Access-Control-Allow-Origin: *`，只对允许来源回显；带 `Origin` 且不在允许范围的非 GET 请求直接 403；`Allow-Headers` 增加 `X-Moss-Desktop-Session`。与严格模式、网关开关无关，作为独立工作包 S1 先行交付 | 消除“本机浏览器任意网页跨站调用回环接口”的现有风险（含 R1 生成、设置修改、非严格模式下的发送） | 只在严格模式下收紧 |

---

## 4. 目标架构

### 4.1 拓扑

```text
┌──────────────┐  WebSocket(音频/控制)   ┌───────────────────────────────┐
│ 小智设备      │◄──────────────────────►│ xiaozhi-esp32-server（自建）   │
│ 实战派ESP32-S3│   OTA 下发服务端地址      │  VAD → ASR → LLM → TTS        │
└──────────────┘                         │  插件 moss_tool（拟新增，Python）│
                                         └──────┬───────────┬────────────┘
                              OpenAI 兼容 HTTP   │           │ HTTP + Bearer(网关令牌)
                     ┌───────────────────────────┘           ▼
                     ▼                              ┌─────────────────────────────────┐
        ┌────────────────────────┐                  │ moss-desktop（Windows）          │
        │ 自有模型服务（云端）     │                  │  /v1/gateway/*（拟新增）         │
        │  ASR / LLM / TTS       │                  │   └► dispatch_tool_call_with     │
        └────────────────────────┘                  │        └► HostBackend → ApiService│
                                                    │  审批服务（拟新增）◄── 桌面 UI 批准 │
        可选：海门 Agent / xiaozhi-client ──MCP──►    │  launch_send_job_for（严格模式）  │
        （作为工具提供方，不监听设备）                  └──────────────┬──────────────────┘
                                                                   │ 串口 / Telnet
                                                                   ▼
                                                           FluidNC 激光雕刻机
```

### 4.2 组件职责

| 组件 | 负责 | 不负责 |
| --- | --- | --- |
| 小智设备 | 唤醒、录音、播放、屏显 | 加工决策；`self.grbl.*` 在融合模式下不得用于完整任务 |
| xiaozhi-esp32-server | 设备会话、VAD/ASR/LLM/TTS 编排、意图到工具调用、把审批状态播报给用户 | 加工状态、确认语义、设备连接参数 |
| 自有模型服务（云端） | 推理；以 OpenAI 协议提供 chat（含 tools）、audio/transcriptions、audio/speech | 任何工具调用；MOSS 不直接访问 |
| `moss_tool` 插件（拟新增） | 把 LLM 函数调用翻译为网关请求；透传 `session_id`、幂等键；把网关结果转为可播报文本 | 自行补 `confirmed=true`；缓存加工状态 |
| MOSS 工具网关（拟新增） | 鉴权、工具白名单、限流、审计、把调用交给 `dispatch_tool_call_with` | 绕过现有 HTTP 门禁 |
| MOSS 审批服务（拟新增） | 生成审批请求、计算动作摘要、发放一次性授权、过期与撤销 | 物理执行 |
| MOSS 桌面 UI | 展示待批准动作（文件预览、材料、功率、速度、目标设备），人工批准/拒绝 | — |
| MOSS 语音中心（拟新增，P4） | 链路模式单选、启停自己启动的 xiaozhi-server、配置向导、状态与日志、待审批入口（§9.4） | 保存模型密钥；关闭外部启动的进程；修改设备固件 |
| `launch_send_job_for` 等执行入口 | 现有全部门禁 + 严格模式下验证授权 | — |
| 海门 Agent / xiaozhi-client（可选，P5） | 额外工具或 Agent 能力 | 设备音频会话；直接调用 MOSS 高风险动作 |

### 4.3 一次加工的端到端时序

> r5 说明：正式链路为 MCP 接入点时，第 3 步改为“xiaozhi-server → 接入点 → MOSS `run_bridge`”，网关删除 `confirmed` 的动作由严格模式在执行层完成（模型填的 `confirmed=true` 不构成授权）；第 7 步没有语音播报批准结果，结果在桌面端显示。以下按启用可选 P2b 网关时的完整形态描述。

1. 用户：“帮我在 3 毫米椴木板上刻‘你好’。”
2. 服务端 ASR → LLM 产生函数调用 `moss_tool(name="laser_workflow_tool", arguments={action:"preview", text:"你好", material:"椴木板", thickness_mm:3})`。
3. 插件 `POST /v1/gateway/tools/call`，网关鉴权后调用 `dispatch_tool_call_with`，返回 `workflow_id`、预览摘要；LLM 播报“已生成预览，约 N 分钟，要开始吗？”。
4. 用户：“开始吧。”LLM 调用 `laser_workflow_tool(action:"confirm_send", workflow_id)`。
5. 融合模式下网关**不转发** `confirm_send`，而是调用审批服务创建请求，返回 `approval_required` + `approval_id` + 人类可读摘要；LLM 播报“请在电脑上确认”。
   创建审批时即解析连接目标（显式参数或 `.env` 默认值；**不允许**串口留空自动选择），把解析后的连接参数存入审批记录；无法得到显式目标时返回 `target_required`，LLM 询问用户或提示到电脑上选择设备。
6. 桌面 UI 弹出审批卡；用户核对后点“批准”。审批服务生成一次性授权，用审批记录中保存的连接参数、以 `CallerContext::Approval{approval_id}` 调用现有 `confirm_send` 路径；`launch_send_job_for` 在严格模式下验证授权未过期、未使用、摘要一致（摘要含解析后的目标）。
7. 后续全部门禁照旧；插件可轮询 `GET /v1/gateway/approvals/{id}` 或调用 `laser_workflow_tool status` 播报进度。
8. 用户说“停”：映射 `laser_workflow_tool cancel`（停止后续发送）或 `laser_safe_action_tool emergency_stop`（`0x18` 经在途作业的同一传输实时写入），二者**不需要审批**。不映射 `laser_off`：加工中它是顺序指令，会被现有逻辑拒绝（§2）。

---

## 5. 接口契约（拟新增）

所有新接口：默认关闭（`MOSS_GATEWAY_ENABLED=false`）；开启后**回环来源也必须带令牌**；响应沿用 [Web API 契约](../developer/api-contracts.md) §1.1 的 `{success, result, detail}` 包装与 `Cache-Control: no-store`。

### 5.1 `POST /v1/gateway/tools/call`

鉴权：`Authorization: Bearer <gateway_token>`。令牌由桌面设置页生成（32 字节 CSPRNG，Base64），服务端只存 SHA-256；不复用局域网配对 token，也不接受配对 token。

请求：

```json
{
  "session_id": "xz-session-placeholder",
  "request_id": "6f1c2d3e-0000-4000-8000-000000000001",
  "tool": "laser_workflow_tool",
  "arguments": {
    "action": "preview",
    "text": "你好",
    "material": "椴木板",
    "thickness_mm": 3
  }
}
```

| 字段 | 规则 |
| --- | --- |
| `session_id` | 必填，1..128 字符；服务端设备会话 ID，仅用于审计与审批关联 |
| `request_id` | 必填，客户端生成的幂等键；同一令牌下 10 分钟内重复提交返回首次结果，不重复执行 |
| `tool` | 必填，必须在网关白名单内（§6.1）；`command_execution_tool`、`open_app_tool`、`shortcut_key_execution_tool` 永不开放 |
| `arguments` | 对象；网关**删除**其中的 `confirmed` 字段后再分发（模型不能确认），删除动作写入审计 |

成功（无需审批）：

```json
{
  "success": true,
  "result": {
    "status": "done",
    "tool_result": { "success": true, "result": "...", "detail": {} },
    "speech": "已生成预览，预计 4 分钟。"
  }
}
```

需要审批：

```json
{
  "success": true,
  "result": {
    "status": "approval_required",
    "approval_id": "apr_placeholder",
    "expires_at": 1791200000.0,
    "summary": "雕刻文字“你好”；椴木板 3 mm；功率 30%，速度 1500 mm/min，1 遍；目标设备 <已配置设备>",
    "speech": "请在电脑上确认后开始雕刻。"
  }
}
```

错误码（拟新增，HTTP 状态 / `detail.code`）：

| code | 状态 | 含义 |
| --- | --- | --- |
| `gateway_disabled` | 404 | 网关未开启 |
| `gateway_unauthorized` | 401 | 令牌缺失或无效 |
| `tool_not_allowed` | 403 | 工具或 action 不在白名单 |
| `rate_limited` | 429 | 超出每会话速率（默认 30 次/分钟） |
| `duplicate_request` | 200 | 幂等命中，`result` 为首次结果 |
| `approval_mismatch` | 409 | 执行时重算的摘要与批准时不一致 |
| `approval_expired` | 410 | 授权超过有效期 |
| `approval_consumed` | 409 | 授权已被使用或已失败 |
| `strict_mode_blocked` | 403 | 严格模式下无授权、无桌面会话的 R3 请求 |
| `origin_not_allowed` | 403 | 浏览器来源不在允许范围（§6.5，默认始终生效） |
| `target_required` | 400 | R3 审批无法得到显式连接目标（串口留空且无 `.env` 默认值） |
| 其他 | 透传 | 现有工具层错误（如 `material_safety_blocked`、`device_busy`）原样返回 |

### 5.1a `GET /v1/gateway/tools`

网关令牌鉴权。返回经 §6.1 白名单过滤后的 `tools/list`（与 `laser_mcp::tools_list_json` 同源），并在每个工具上附加 `x-moss-risk: R0..R3`，供插件动态生成函数描述；R4 不出现。

### 5.2 `GET /v1/gateway/approvals/{approval_id}`

网关令牌鉴权。返回 `{approval_id, status, summary, created_at, expires_at, decided_at, workflow_id, outcome}`；`status ∈ pending | approved | rejected | expired | executed | failed`。`outcome` 透传执行后的 `send_result` 摘要，不含文件路径。

### 5.3 桌面审批接口（仅桌面会话）

| 路由 | 用途 |
| --- | --- |
| `GET /v1/approvals?status=pending` | 列出待批准请求 |
| `POST /v1/approvals/{id}/approve` | 批准；body `{digest}` 必须与当前重算的摘要一致 |
| `POST /v1/approvals/{id}/reject` | 拒绝；body `{reason?}` |

鉴权：`X-Moss-Desktop-Session: <desktop_session>`。该凭据由 Tauri 命令（拟新增 `get_desktop_session`，与现有 `get_backend_origin` 并列）在每次启动时生成，只存在于桌面 WebView 内存；浏览器同源页、网关令牌、配对 token 都不能调用这组接口。桌面页经 HTTP 访问后端，属于跨源请求，因此该头必须加入 `Access-Control-Allow-Headers`（D11）；后端校验通过后构造 `CallerContext::Desktop`（§6.4）。独立 `moss-host serve` 没有桌面会话，因此在严格模式下**无法批准**，只能拒绝或过期。

### 5.4 审批摘要（digest）

`digest = SHA-256(canonical_json(payload))`，`canonical_json` 为键名排序、无多余空白的 UTF-8 JSON。`payload`：

```json
{
  "kind": "workflow_confirm_send",
  "workflow_id": "wf_placeholder",
  "gcode_sha256": "<冻结快照中的 SHA-256>",
  "material": "椴木板",
  "thickness_mm": 3,
  "params": { "power": 300, "power_scale": 1000, "feed_rate": 1500, "passes": 1 },
  "target": { "mode": "network", "host_fingerprint": "<连接目标的哈希>" },
  "send_mode": "stream"
}
```

批准时与执行时各重算一次；任一字段变化（重新预览、换设备、改发送模式）都使旧授权失效，返回 `approval_mismatch`。`host_fingerprint` 对**解析后**的连接描述（`ConnectionSpec::describe()` 的结果）取 SHA-256，而不是对请求原文；摘要中只放哈希。执行所需的连接参数另存于审批记录的 `connection` 列（§7.1），执行时使用该列而不是重新从请求解析，保证“批准的就是执行的”。

### 5.5 授权的消费

- 授权有效期默认 120 秒（`MOSS_APPROVAL_TTL_SECONDS`，范围 30..600）。
- 单次使用：执行入口在同一 SQLite 事务中把状态从 `approved` 改为 `executed`，成功后才进入 `launch_send_job_for` 的设备阶段；事务失败即拒绝。
- 执行失败（如 `device_preflight_failed`）状态记为 `failed`，**不可重用**；用户需重新发起。
- 进程重启后所有 `pending/approved` 标为 `expired`。

---

## 6. 权限模型

### 6.1 工具分级（网关白名单）

| 级别 | 工具 / action | 网关行为 |
| --- | --- | --- |
| R0 只读 | `check_laser_connection_tool`；`laser_workflow_tool status/recent`；`laser_safe_action_tool status`；`laser_grbl_tool`/`laser_network_grbl_tool` 的 `status/check/probe`；`material_params_tool` 读取类；`recommend_laser_params_tool` | 直接转发 |
| R1 生成，不触设备 | `laser_workflow_tool preview`；文字生成三件套；`run_calibration_grid_tool`（无 `calibration_id`）；`select_calibration_cell_tool`；`refine_laser_params_from_feedback_tool`；`regenerate_text_laser_task_tool`（不含采用） | 直接转发 |
| R2 降低风险 | `laser_workflow_tool cancel`；`laser_grbl_tool cancel`；`laser_safe_action_tool emergency_stop / soft_reset / laser_off`；进给/功率倍率中的 `*_minus_10`、`*_reset` | 直接转发并在网关侧补 `confirmed=true`（这是唯一允许网关补确认的类别，因为拒绝停机比误停更危险）。注意：加工中只有实时字节类（`emergency_stop`、`soft_reset`、倍率）会生效，`laser_off` 会被拒绝；提示词中“停”只映射 `cancel` / `emergency_stop` |
| R3 运动或出光 | `laser_workflow_tool confirm_send/send/resume`；`start_tuned_job_tool`；`run_calibration_grid_tool` 带 `calibration_id`；`laser_safe_action_tool unlock/home/set_origin/absolute_move/relative_move`；`*_plus_10` 倍率；`regenerate_text_laser_task_tool` 采用反馈 | 创建审批请求，不转发 |
| R4 永不开放 | `command_execution_tool`、`open_app_tool`、`shortcut_key_execution_tool`、`open_jsjds_website_tool`；材料库删除/覆盖 | `tool_not_allowed` |

说明：`*_plus_10` 倍率现有语义“不需确认”，但会提高功率或速度，融合模式下归入 R3。此为对语音入口的收紧，网页/桌面行为不变。

### 6.2 严格模式（`MOSS_FUSION_STRICT=true`）

开启后在**执行层**（不是路由层）依据 `CallerContext`（§6.4）检查；“来自桌面会话”指 `CallerContext::Desktop`，“携带有效审批授权”指 `CallerContext::Approval`：

| 入口 | 现有要求 | 严格模式追加 |
| --- | --- | --- |
| `launch_send_job_for`（workflow `confirm_send/send/resume`、`/api/text-task/send`、材料矩阵 `send`） | `confirmed=true` + 全部门禁 | 必须携带有效审批授权，或请求来自桌面会话 |
| `/api/safe-action` 的 R3 动作 | `confirmed=true` | 同上 |
| `/api/device/framing` | 无 | 同上（它真实驱动机器，见安全入口 §3） |
| 旧 MCP 桥 `run_bridge` | 模型可传 `confirmed=true` | R3 一律返回 `approval_required`，与网关一致 |
| 回环 HTTP（无桌面会话头） | 免鉴权 | 构造为 `CallerContext::Loopback`，R3 不放行（CORS 与来源校验按 D11 始终生效） |
| R2 停止类 | — | 不追加任何条件，保证随时可停 |

严格模式默认 `false`，以保证现有官方链路与网页用户行为不变；**开启网关时强制开启严格模式**（`MOSS_GATEWAY_ENABLED=true` 且 `MOSS_FUSION_STRICT=false` 时启动失败并提示）。

### 6.3 身份

| 身份 | 来源 | 可做 |
| --- | --- | --- |
| 桌面会话 | Tauri 启动时生成，只在 WebView 内存 | 一切，含批准 |
| 网关令牌 | 设置页生成，配置到服务端插件 | R0–R3（R3 仅生成审批） |
| 局域网配对 token | 现有 `AuthManager` | 现有行为；严格模式下 R3 需审批 |
| 旧 MCP 桥 / MCP 接入点 | 出站 WebSocket | 同网关令牌 |
| 回环无凭据 | 本机其他进程或网页 | 现有行为；严格模式下 R3 需审批 |

### 6.4 调用方上下文 `CallerContext`（拟新增）

```text
enum CallerContext {
    Desktop,                         // X-Moss-Desktop-Session 校验通过
    Approval { approval_id: String },// 审批服务在桌面批准后发起的执行
    Gateway  { session_id: String }, // /v1/gateway/* 网关令牌
    McpBridge,                       // InProcessBackend（run_bridge 转发）
    Lan      { pairing_id: String }, // 局域网配对 token
    Loopback,                        // 回环且无任何凭据
}
```

落地规则：

1. **入口构造，不信任请求体**：`ApiService::serve` 按头部与来源构造上下文；`handle_http_from` 增加 `ctx` 参数，`handle_http`（无参版本）保留给测试并固定为 `Loopback`。请求体中的任何身份类字段在 HTTP 入口一律删除。
2. **`InProcessBackend` 显式声明身份**：新增 `ApiService::handle_http_with_ctx(request, ctx)`，`InProcessBackend` 传 `McpBridge`，网关分发传 `Gateway`，不再借用回环语义。`HostBackend` trait 签名不变，身份由各实现自带。
3. **向下传递**：`handle` → workflow / text-task / material-lab / safe-action / framing 处理函数 → `launch_send_job_for`、`handle_safe_action` 增加 `ctx: &CallerContext` 参数；严格模式检查集中在一个函数 `require_r3_authority(ctx, kind, digest)`，所有 R3 执行入口调用它，避免散落判断。
4. **严格模式关闭时**：上下文只用于审计，不改变任何现有判定，保证 A-40 回归。
5. **工作量提示**：`crates/laser-api/src/lib.rs` 约 290 KB，涉及上述入口的函数签名需逐一修改；建议作为 P1 的第一个独立提交（P1-0），先做纯签名改造并让现有测试全绿，再叠加严格模式逻辑。

### 6.5 CORS 收紧与来源校验（D11，默认始终生效）

用户已决定不允许任意来源访问。本节规则**不依赖** `MOSS_FUSION_STRICT` 或网关开关，作为独立工作包 S1 实施。

允许来源（精确匹配 scheme + host + port）：

| 来源 | 用途 |
| --- | --- |
| （前置）回环连接的 `Host` 须为 IP 字面量 / `localhost` / `MOSS_ALLOWED_ORIGINS` 中的主机名 | 防 DNS 重绑定，见本节末尾 |
| `http://tauri.localhost` | Tauri 2 在 Windows 与 Android 上的内嵌页（以 P1 前实测为准） |
| `tauri://localhost` | Tauri 2 在 macOS/Linux 上的内嵌页 |
| 与后端同源（`Origin` 的 host:port 等于请求 `Host`） | MOSS 自身提供的页面，含局域网浏览器访问 `/`、`/draw`、`/material-lab` |
| `http://127.0.0.1:5177`、`http://localhost:5177`、`http://127.0.0.1:5178`、`http://localhost:5178`，以及 Vite 默认的 `5173` | 前端 `npm run dev` / `preview` 的固定端口（见 `frontend/package.json`）；`frontend/src/authSession.js` 同样把 5173 视为开发端口，S1 实施时一并允许；仅回环来源请求时生效 |
| `MOSS_ALLOWED_ORIGINS` | 额外来源，逗号分隔，默认空；只用于排障，不得写 `*`（写 `*` 时启动报错） |

规则：

1. **响应头**：请求 `Origin` 在允许范围内时回显该值并加 `Vary: Origin`；否则不返回任何 `Access-Control-Allow-*` 头。`Access-Control-Allow-Headers` 为 `Content-Type, Authorization, Accept, X-Moss-Desktop-Session`。
2. **来源校验**：仅收紧响应头不够——`Content-Type: text/plain` 的 POST 属于“简单请求”，浏览器不发预检就会送达，后端照样执行。因此请求带 `Origin` 且不在允许范围时，除 `GET`/`HEAD` 外一律 403 `origin_not_allowed`，不进入路由；`OPTIONS` 预检对不允许来源返回 403 且不带 CORS 头。
3. **不带 `Origin` 的请求不受影响**：服务端插件、xiaozhi-client、`InProcessBackend`、curl、局域网配对客户端（非浏览器）照常走现有鉴权。
4. **行为变更**：这是对现有对外行为的收紧，实施时须在 `CHANGELOG.md` 显式标注“破坏性变更”，并更新现有断言 `Access-Control-Allow-Origin: *` 的测试（`laser-api` 中的 HTTP 适配器健康检查与 `http_adapter_handles_cors_options_preflight`），改为断言允许来源回显、不允许来源 403。

即使来源校验被绕过，严格模式下 R3 仍由 §6.2 在执行层拦截。

**实施状态（2026-10-06，S1 代码已完成，构建待本机验证）**：`laser-api` 新增 `is_allowed_origin`，`handle_http_from` 在路由前校验并统一注入 CORS 头，响应构造函数不再写 CORS 头；`MOSS_ALLOWED_ORIGINS` 由 `laser_runtime::config::parse_allowed_origins` 解析（同时拒绝缺少 scheme 或带路径的项），`laser-host` 启动时读取。

**DNS 重绑定防护（2026-10-06 已实施）**：“与 `Host` 同源”规则本身挡不住 DNS 重绑定——攻击者域名解析到 127.0.0.1 后，`Origin` 与 `Host` 都是攻击者域名，浏览器视为同源，连 GET 响应都能读到。因此在来源校验之前增加一步：**回环连接**的 `Host` 必须是 IP 字面量、`localhost` 或 `MOSS_ALLOWED_ORIGINS` 中的主机名，否则任何方法一律 403 `host_not_allowed`（`is_allowed_loopback_host`）。攻击者无法让浏览器在访问自己域名时发出 `Host: 127.0.0.1`，所以此规则可封住重绑定。非回环连接不做此项：局域网访问本就需要 Bearer，token 存在 MOSS 页面自己的 sessionStorage 里，重绑定页面拿不到。P1 的 `X-Moss-Desktop-Session` 会话凭据仍按计划实施，作为第二道防线。

---

## 7. 数据模型（schema v4，拟新增）

### 7.1 `approvals`

| 列 | 类型 | 说明 |
| --- | --- | --- |
| approval_id | TEXT PK | `apr_` + 随机 |
| status | TEXT | pending/approved/rejected/expired/executed/failed |
| kind | TEXT | `workflow_confirm_send`、`calibration_send`、`safe_action`、`framing`… |
| target_record | TEXT | workflow_id / calibration_id |
| digest | TEXT | §5.4 |
| summary | TEXT | 人类可读摘要，不含路径和凭据 |
| source | TEXT | gateway / mcp_bridge / lan |
| session_id | TEXT | 服务端会话 ID |
| request_id | TEXT | 幂等键，`UNIQUE(source, request_id)` |
| connection | TEXT | JSON，创建审批时解析出的显式连接参数（模式、串口/主机、端口、波特率等）；执行时使用，不重新解析。本地库内保存，接口与摘要不返回明文 |
| created_at / decided_at / expires_at / updated_at | REAL | Unix 秒 |
| outcome | TEXT | JSON，执行结果摘要 |

### 7.2 `gateway_requests`（幂等缓存）

`(token_hash, request_id)` 主键，`response` JSON，`created_at`；超过 10 分钟的行在启动与每小时清理。

### 7.3 `audit_log` 开始写入

复用现有表结构（[数据库结构](../developer/db-schema.md) §3.11），新增 `laser-contracts::AuditDetail` 强类型。写入时机：审批创建/批准/拒绝/过期/执行、网关删除 `confirmed` 字段、R4 拒绝、严格模式拦截。`client` 列取 `desktop`/`gateway`/`mcp`/`pairing_id`。审计写入失败不阻断 R2 停止类动作，但阻断 R3。

迁移规则遵循现有 schema 迁移约定：在 `RuntimeDb` 迁移函数中追加 `if current < 4 { ... }` 分支，单事务、可重复执行、失败回滚并阻止业务读写。回滚方案见 §9.3。

---

## 8. 故障与恢复

| 场景 | 行为 |
| --- | --- |
| 模型服务宕机/超时 | 服务端按自身配置报错或降级播报；MOSS 不受影响 |
| xiaozhi-esp32-server 宕机 | 设备离线；网页/桌面加工、急停照常；待批准请求到期自动 `expired` |
| 插件重复提交 | `request_id` 幂等命中，返回首次结果 |
| 批准后、进入设备阶段前 MOSS 崩溃 | 重启后 `approved` → `expired`，workflow 仍为 `pending`；需重新批准 |
| 发送中 MOSS 崩溃 | 现有 `recover_after_restart` 将 workflow 标 `failed`；审批记 `executed`，`outcome` 写“执行结果未知”；**不重放**；提示用户到机器旁确认 |
| 设备断线、复位横幅、报警 | 现有“结果未知”语义；网关返回该状态，LLM 播报“请到机器旁确认” |
| 审批卡无人处理 | TTL 到期 `expired`；插件轮询得到后播报 |
| 网关令牌泄露 | 设置页“重置令牌”立即使旧令牌失效；最坏后果为生成 R3 审批请求，无法自行执行 |

---

## 9. 部署、迁移与回滚

### 9.1 部署形态（Windows）

| 组件 | 形态 | 由谁启动 |
| --- | --- | --- |
| moss-desktop | 现有 exe | 用户 |
| xiaozhi-esp32-server | 先用**最简化部署**（配置文件、单智能体；模型全走云端 API 时上游建议 2 核 2 GB 即可）；需要智控台再换全模块。Docker 或源码按 P0 结论选一种 | MOSS 语音中心一键启停（P4，§9.4.3）；也可由用户在外部手动启动，此时 MOSS 只显示状态 |
| MCP 接入点 `mcp-endpoint-server`（正式链路） | 上游独立项目，Docker 或源码运行，默认端口 8004；xiaozhi-server 的 `mcp_endpoint` 与 MOSS 的 `mcp_endpoint` 填同一个“单模块接入点地址”（含 token，不写进文档） | 用户；语音中心可选托管（§9.4.3） |
| ASR/LLM/TTS | 用户自有云端服务（OpenAI / Anthropic 协议）；在 xiaozhi-server `config.yaml`（或智控台）中以 `type: openai` 填地址与密钥 | 用户；MOSS 不探活、不保存密钥 |
| `moss_tool` 插件（可选 P2b） | 放入服务端 `plugins_func/functions` 的单个 Python 文件，并加入 `Intent.function_call.functions`；配置项 `MOSS_GATEWAY_URL`、`MOSS_GATEWAY_TOKEN` | 随服务端加载 |
| 设备固件 | 小智固件的 OTA 接口地址指向自建服务端 | 用户刷写/配置（需另行授权） |

网络注意：网关与现有路由共用 MOSS 的同一个 HTTP 监听（不另开端口）。服务端若跑在 **Docker** 中，请求经 Docker 网桥到达，来源**不是回环**，MOSS 的监听地址必须覆盖该网卡，这会同时把现有接口暴露给局域网（非回环接口仍需 Bearer，见 api-contracts §5）。因此首选**源码部署在同一台电脑**，请求来自回环；必须用 Docker 时，用 Windows 防火墙把 MOSS 端口限制为本机与 Docker 网段，并保持严格模式开启。

拟新增配置项（全部默认关闭/空，不写真实值）：

| 名称 | 默认 | 说明 |
| --- | --- | --- |
| `MOSS_GATEWAY_ENABLED` | `false` | 开启工具网关 |
| `MOSS_FUSION_STRICT` | `false` | 严格模式；网关开启时必须为 `true` |
| `MOSS_APPROVAL_TTL_SECONDS` | `120` | 审批有效期 |
| `MOSS_GATEWAY_RATE_PER_MIN` | `30` | 每会话速率 |
| `MOSS_ALLOWED_ORIGINS` | 空 | 在内置允许来源（§6.5）之外额外允许的浏览器来源，逗号分隔；只用于排障，禁止 `*` |

### 9.2 迁移

1. **P2a 并行试运行**：保留官方链路配置备份；把 `mcp_endpoint` 临时指向自建接入点，只做查询与预览（R0/R1）。此阶段 MOSS 未开严格模式，**不得进行真实发送**——与官方链路一样，模型仍可自行填 `confirmed=true`。
2. **P1 完成并开启严格模式后**，再允许经自建链路发起加工（经桌面审批）。
3. 自建链路验收通过（§11）后，由用户决定是否删除官方配置备份。**不自动改用户设置**。
4. 提示词：官方控制台的 2000 字限制不再适用，但 `docs/integration/xiaozhi-prompts/latest.md` 仍是唯一维护处；自建服务端的系统提示另存 `history/<日期>-self-hosted.md` 并在 `latest.md` 注明两套版本的适用范围。

### 9.3 回滚

| 层 | 回滚方式 |
| --- | --- |
| 设备 | 固件 OTA 地址改回官方 |
| MOSS 配置 | `MOSS_GATEWAY_ENABLED=false`、`MOSS_FUSION_STRICT=false`、`mcp_endpoint` 恢复官方地址 |
| 数据库 | **不需要降级脚本**：`RuntimeDb` 迁移只执行 `current < N` 的步骤、不拒绝更高版本（§2），旧版程序打开 v4 库时忽略 `approvals`、`gateway_requests` 两张新表，旧表结构未改。回滚前仍建议复制一份 `moss.sqlite` 备份。A-41 覆盖此结论 |
| 代码 | 网关与审批位于独立模块；严格模式关闭时 `CallerContext` 只用于审计，行为与现状一致 |

### 9.4 语音中心与进程管理（P4，D12）

#### 9.4.1 页面结构

桌面端新增路由 `/voice`（Android 构建不显示入口，见 §9.4.7）：

```text
┌ 语音中心 ──────────────────────────────────────────┐
│ 语音链路： ○ 官方小智   ● 自建服务端                   │  单选，互斥
│ 服务端    ● 运行中（由 MOSS 启动）  [启动] [停止] [日志]  │
│ 设备      ● 在线                                      │
│ 模型      大模型 ✓ · 语音识别 ✓ · 语音合成 ✓ · 上次测试 3.2 s │
│ 网关      已开启 · 严格模式 · [重置令牌]                │
│ 待审批(1) 雕刻“你好” · 椴木板 3 mm        [去审批]      │
│ [配置向导]                                             │
└──────────────────────────────────────────────────────┘
```

#### 9.4.2 链路模式单选

| 模式 | MOSS 侧自动设置 | 需要用户在 MOSS 之外做的 |
| --- | --- | --- |
| 官方小智 | `mcp_endpoint` 恢复为官方地址（从切换前的备份取）；网关关闭；由 MOSS 启动的服务端提示是否停止 | 设备 OTA 地址改回官方 |
| 自建服务端 | 严格模式开启（必选）；`mcp_endpoint` 指向自建接入点；按需启动接入点与服务端；仅当启用可选 P2b 时开启网关 | 设备 OTA 地址指向自建服务端（`http://<服务端地址>:8003/xiaozhi/ota/`） |

规则：

1. 两种模式**互斥**，切换时旧链路的 MCP 桥先断开，再接新链路，保证同一时刻只有一个语音编排者（D1）。
2. MOSS **不能修改设备固件或 OTA 地址**；切换后页面持续提示“设备当前连接的服务端”与所选模式是否一致（依据服务端是否看到设备在线判断，无法判断时显示“未知”）。
3. 官方模式下严格模式是用户可选项（默认关闭，保持现状）；页面说明官方链路下模型可自行确认发送的风险，并建议开启。
4. 切换会写 `settings.json`，属于持久化配置变更，切换前弹确认框并列出将改动的项。

#### 9.4.3 服务端启停

启停对象：xiaozhi-server（必有），`mcp-endpoint-server`（若以源码运行；以 Docker 运行时只显示状态）。启动顺序：接入点 → xiaozhi-server → MOSS 重连桥；停止顺序相反。启动前检查 `data/.config.yaml` 是否存在，缺失时引导进入配置向导。

| 项 | 规则 |
| --- | --- |
| 配置来源 | `settings.json` 新增 `voice.server_dir`（`main/xiaozhi-server` 目录）、`voice.python`（Python 3.10 解释器路径）、`voice.args`（默认 `app.py`）；可选 `voice.endpoint_dir` 与对应启动参数（源码运行接入点时）；不在仓库或文档中写真实路径 |
| 启动 | `std::process::Command` 启动子进程，工作目录为 `server_dir`；记录子进程句柄与启动时间；同一时刻最多一个由 MOSS 启动的实例 |
| 外部实例 | 启动前探测服务端端口；已被占用且不是 MOSS 的子进程时，标为“外部运行”，**不提供停止按钮** |
| 停止 | 只停止 MOSS 持有句柄的子进程：先请求正常退出，超时 5 秒后强制结束；Windows 使用 Job Object 确保子进程树一并结束 |
| MOSS 退出 | 托盘退出、窗口关闭退出时弹出“同时停止语音服务端？”（默认是）；panic、控制台关闭等异常退出路径由 Job Object 的“随父进程关闭”保证不留孤儿进程。`ShutdownCoordinator` 的急停顺序不变：先 `abort_all_active_jobs`，再处理服务端 |
| 崩溃 | 子进程意外退出时页面显示退出码与最近日志；**不自动重启**（避免崩溃循环），由用户点击启动 |
| 日志 | 子进程 stdout/stderr 写入运行目录 `logs/xiaozhi-server.log`，按 5 MB × 3 份轮转；写入前对形如密钥的字段做脱敏（`api_key`、`Authorization`、`token` 后的值替换为 `***`） |

服务端进程管理代码放在 `apps/desktop/src-tauri`（或新建桌面专用模块），不进入 `laser-api`，以免影响 `moss-host` 与 Android。

#### 9.4.4 模型配置向导

1. 向导分为**大模型、语音识别、语音合成三个独立分区**，每个分区各自填写地址、密钥、模型（三者可以来自不同服务商或同一云端）。密钥输入框为密码样式，不回显已保存的密钥。字段与上游配置的对应关系：

   | 分区 | 写入位置（`data/.config.yaml`） | 字段 | 说明 |
   | --- | --- | --- | --- |
   | 大模型 | `LLM.MossLLM` | `type: openai`、`url`、`api_key`、`model_name` | `url` 填基础地址（如 `https://<host>/v1`） |
   | 语音识别 | `ASR.MossASR` | `type: openai`、`base_url`、`api_key`、`model_name`、`output_dir: tmp/` | `base_url` 是**完整**的 `/audio/transcriptions` 地址；向导可由基础地址自动补全并显示最终值 |
   | 语音合成 | `TTS.MossTTS` | `type: openai`、`api_url`、`api_key`、`model`、`voice`、`speed`、`output_dir: tmp/` | `api_url` 是**完整**的 `/audio/speech` 地址 |
   | 选择 | `selected_module` | `LLM: MossLLM`、`ASR: MossASR`、`TTS: MossTTS`、`Intent: function_call` | 其他模块（VAD 等）保持上游默认 |
   | 接入点 | 顶层 `mcp_endpoint` | 单模块接入点地址 | 与 MOSS 的 `mcp_endpoint` 相同 |

   某一分区留空时，不改该模块的现有配置（例如只换大模型、语音仍用上游默认服务）。
2. **测试连接**：MOSS 用内存中的值各发一次最小请求（大模型带一个工具定义，检查是否返回 `tool_calls`；语音识别用内置的一段短音频；语音合成合成一句短文本），显示是否成功与耗时。请求结束立即丢弃密钥；测试请求超时 15 秒，不跟随重定向。
3. **写入**：用户确认后，向导以 YAML 读-改-写方式只更新上表字段，保留文件中其他内容与注释，写入前备份为 `data/.config.yaml.bak`，写入后重新解析校验。MOSS 的 SQLite、`settings.json`、日志中**都不保存密钥**。
4. 写入后提示“需要重启服务端生效”，提供重启按钮（仅对 MOSS 启动的实例）。
5. 服务端若使用全模块部署（智控台管理配置），向导只做“测试连接”，不写文件，并提示到智控台修改。

#### 9.4.5 状态来源

| 状态 | 来源 | 无法获得时 |
| --- | --- | --- |
| 服务端 | 子进程句柄 + 端口探测 | “未运行” / “外部运行” |
| 设备在线 | 接入点 `/mcp_endpoint/health?key=…` 返回的 `robot_connections`（>0 表示有设备会话经服务端连到接入点）；`tool_connections` 同时反映 MOSS 桥是否在线 | “未知”，不猜 |
| 模型 | 最近一次向导测试结果与时间 | “未测试” |
| 网关、严格模式、待审批 | MOSS 内部状态 | — |

#### 9.4.6 接口（拟新增，仅桌面会话）

均要求 `CallerContext::Desktop`，网关令牌、配对 token、回环无凭据一律 403。

| 路由 | 用途 |
| --- | --- |
| `GET /v1/voice/status` | 模式、服务端状态、设备状态、模型测试结果、网关与审批计数 |
| `POST /v1/voice/mode` | `{mode: "official" \| "self_hosted"}`，执行 §9.4.2 |
| `POST /v1/voice/server/start`、`/stop`、`/restart` | §9.4.3 |
| `GET /v1/voice/server/log?tail=200` | 已脱敏的日志尾部 |
| `POST /v1/voice/models/test` | 向导测试，请求体含密钥，只在内存使用，响应不回显 |
| `POST /v1/voice/models/save` | 写入服务端配置文件 |

Tauri 端也可直接用 `#[tauri::command]` 实现上述操作而不走 HTTP；若走 HTTP，必须满足上面的会话要求和 §6.5 的来源校验。

#### 9.4.7 跨端

Android 构建不编译进程管理代码（`cfg(not(target_os = "android"))`），前端不显示语音中心入口；`/v1/voice/*` 在 Android 与 `moss-host serve` 中返回 404。

#### 9.4.8 打包（后续再定）

首版交付形态为“MOSS 安装包（§9.6）+ 用户自备的服务端源码目录”，由配置向导引导填写目录与 Python 路径；安装包不捆绑服务端，卸载也不删除这些外部目录。把 Python 运行环境打进安装包（体积与依赖问题较多）在链路稳定后单独评估，不在 P4 范围内。

### 9.5 桌面端前端改造（r6）

交互原型：`frontend/ui-preview-voice-center.html`（演示数据，不连后端；含 8 个测试场景 TC-01..TC-08）。原型只表达布局与交互，实施时以本节与 [前端行为契约](../developer/frontend-behavior.md) 为准。

#### 9.5.1 新增“语音中心”页 `/voice`（P4-6）

- 导航：放在“创作画板”之后，作为第四个标签页（徽标 `VOICE`）；`main.jsx` 按 `pathname` 前缀增加 `/voice` 路由，`popstate` 同步方式与现有三页一致。Android 构建不显示该标签（§9.4.7）。
- 页面内容（自上而下）：
  1. **语音链路**：官方小智 / 自建服务端二选一，互斥；切换前弹确认框列出将改动的设置；下方持续显示设备 OTA 一致性提示（§9.4.2）。
  2. **四张状态卡**：服务端（带启动、停止、重启按钮，外部启动的实例不显示停止）、MCP 接入点（端口、工具连接数、设备会话数）、设备（依据接入点 `robot_connections`）、严格模式（自建模式下不可关闭）。
  3. **模型配置**：大模型、语音识别、语音合成三栏分别填写地址、密钥、模型（语音合成另有音色、语速）；每栏先“测试连接”成功才能“保存到服务端配置”；显示最终写入的完整地址；密钥框不回显（§9.4.4）。
  4. **审批记录**：时间、内容、来源、状态（待批准 / 已批准 / 已拒绝 / 已过期 / 摘要不一致 / 已完成 / 已取消 / 急停）。
  5. **服务端日志**：读取 `/v1/voice/server/log`，只显示已脱敏内容。
- **MCP 地址迁移**：`SettingsModal` 中的 MCP 地址输入移除，改为一行说明“小智连接在语音中心设置”并提供跳转；`/api/settings` 的 `mcp_endpoint` 字段保留以兼容旧前端与 `moss-host`，但桌面端只从语音中心修改，避免两处可改。

#### 9.5.2 全局审批卡（P1-7）

- 挂在 `main.jsx` 的 `App` 根组件，与 `AuthModal`、`CloseConfirmModal` 同级，因此**任何页面**都会弹出。轮询 `GET /v1/approvals?status=pending`（与现有 `/api/system/info` 同一 4 秒节奏）；有新请求时自动弹出，关闭后可从顶栏“审批”按钮重新打开。
- 卡片内容：刀路预览图、内容、材料与厚度、功率（含标尺）/速度/遍数、尺寸与预计时长、目标设备（只显示连接方式与设备名，不显示 IP）、来源（官方或自建、会话）、摘要前后缀、120 秒倒计时条。
- 交互规则：
  - 必须勾选“我在机器旁，材料已放好、原点已确认，排风已开启”，“批准并开始”才可点击；
  - 批准时提交 `{digest}`，服务端返回 `approval_mismatch` / `approval_expired` 时在卡内显示原因，按钮收起为“关闭”，不发送任何指令；
  - 倒计时归零自动关闭并记为“已过期”；
  - “拒绝”立即生效；“稍后处理”只关闭卡片，请求仍在等待。
- 批准后进入现有发送覆盖层（`SendOverlay`），**`STAGE_MAP` 四步映射原样保留**（前端行为契约 §2）；发送全程显示“急停 0x18”与“取消后续发送”，急停说明文字保持“0x18 不代替硬件急停”。

#### 9.5.3 顶栏真实状态（独立修复 S2，与融合无关，可先行交付）

现状问题（2026-10-05 读代码核实，`crates/laser-api/src/lib.rs` 的 `/api/system/info` 分支）：

| 字段 | 现状 | 问题 |
| --- | --- | --- |
| `mcp.configured` | 有 `mcp_endpoint` **或**有任何活动即为真 | 只表示“已配置”，前端却据此显示 CONNECTED |
| `mcp.client_info` | 无真实客户端时，已配置就返回 `xz-mcp-broker v0.0.1` | 编造的客户端名 |
| `mcp.recent_events` | 无真实事件时返回写死 `16:00:02` 时间戳的 4 条日志 | 编造的事件 |
| `mcp.messages_count` | `max(真实值, 3)` | 编造的最小值 |
| `mcp.tools_count` | 固定 20 | 未开开发者开关时实际为 19 |
| `mcp.last_request` | 空时返回 `tools/list (查询工具清单)` | 编造的最近请求 |

修复要求：

1. 后端只返回真实遥测：新增 `mcp.connected`（桥当前 WebSocket 是否已握手）；`client_info`、`recent_events`、`last_request` 无真实数据时返回空值；`messages_count` 返回真实计数；`tools_count` 取 `tools_list_json(developer_mode)` 的实际长度。其余展示性字段（`core_tool`、`safety_gate`、`estop_channel`）可保留，但须与 safety 文档一致。
2. 新增 `voice.mode`（`official` / `self_hosted`）与 `voice.device_sessions`（自建模式下来自接入点 health，未知为 `null`），供顶栏使用。
3. 顶栏“小智”胶囊四种状态，并标注链路：

   | 状态 | 判定 | 样式 |
   | --- | --- | --- |
   | 未配置 | 没有 `mcp_endpoint` | 灰 |
   | 未连接 | 已配置但 `connected=false` | 红 |
   | 等待设备 | 已连接，自建模式下 `device_sessions=0` | 金 |
   | 已连接 | 已连接；官方模式或 `device_sessions≥1` | 绿（官方用青色） |

   文案示例：“小智 · 自建 · 已连接”。官方模式无法得知设备会话，`connected=true` 即视为已连接。
4. 顶栏新增“审批”按钮：无待批准时为灰色；有待批准时变红并显示数量角标，点击打开审批卡。
5. 修复属于对外行为变化，按 `AGENTS.md` 记 CHANGELOG（标注：此前状态显示不可信）；同步 `api-contracts.md` 的 `/api/system/info` 字段说明。

**实施状态（2026-10-06，S2）**：第 1、2、3、5 条已实施——`McpTelemetry` 增加 `connected` 与真实消息计数，桥连接期间由守卫置真、断开置假；`/api/system/info` 改为 `mcp_status_json` 只输出真实数据，新增 `mcp.connected` 与 `voice`；前端 `state/mcpStatus.js` 判定四种胶囊状态，控制台抽屉与偏好设置去掉写死的默认值。第 4 条“审批”按钮依赖审批服务，随 P1 实施。

#### 9.5.4 请求携带桌面会话凭据（P1-6/P1-7）

- `authSession.js` 在 `initializeBackendOrigin` 时，Tauri 环境下额外调用 `get_desktop_session` 取得会话标识并只保存在内存（不写 `localStorage`）。
- `api/client.js` 的 `apiFetch` 统一在请求头加 `X-Moss-Desktop-Session`；非 Tauri 环境（浏览器、局域网、Android 浏览器访问）不加。
- 收到 403（缺少或无效会话，如后端重启后）时，重新获取一次会话标识并重试一次；仍失败则提示“桌面会话已失效，请重启 MOSS”。
- 前提：S1 已把该头加入 `Access-Control-Allow-Headers`。

#### 9.5.5 前端整体改版（UI-2，取代原 UI-1 视觉提亮；**2026-10-06 定稿并已实施**）

**实施状态**：令牌与基础组件、左侧导航与顶栏（含常驻急停）、雕刻工作台三栏、创作画板、材料库表格、弹窗、可折叠卡片与偏好设置分区、桌面端导出（`save_export_file`）均已实施，用户本机测试与构建通过；视觉基线见 `frontend/tests/visual-baseline/`。需实机的重复发送防呆与顶栏急停见实施清单 §2a（HW-01、HW-02）。Geist 字体待提供字体文件后自托管；组件内联样式迁移为类名作为后续重构。

原型：`frontend/ui-preview-forge-v2.html`（演示数据，不连后端）。顶部“功能对照清单”列出原前端 83 项按钮与功能（其中 2 项改了位置或显示方式）+ 5 项新增，可逐项“定位”高亮验证。该原型是当前讨论的设计基线，**用户已明确后续还要修改**：实施前以用户最终确认的原型版本为准，本节随原型更新。

**A. 不变的约束**

- 原有按钮、输入、弹窗与流程全部保留，行为与 [前端行为契约](../developer/frontend-behavior.md) 一致（`STAGE_MAP`、重复发送防呆、`localStorage` 键、24 字上限、串口真实枚举、材料下拉只暴露名称与厚度、`BroadcastChannel` 同步、关闭窗口选择等）。
- 只改信息架构与视觉，不改 API 调用、参数与门禁；改版不得顺带改变任何业务行为。
- 新增项只有：语音中心（§9.5.1）、全局审批卡（§9.5.2）、顶栏真实状态与审批按钮（§9.5.3）、顶栏常驻急停（与监视舱内原急停按钮并存，调用同一 `0x18` 安全动作）。

**B. 信息架构**

| 区域 | 设计 |
| --- | --- |
| 导航 | 顶部横向标签改为左侧竖向导航：雕刻工作台、创作画板、材料实验室、语音中心；底部为偏好设置、配对认证。窄屏（≤900px）折叠为顶部横排 |
| 顶栏 | 页面标题（金属渐变字）+ 硬件状态、小智状态（四态，标注官方/自建）、审批按钮（红色数量角标）、常驻急停 0x18 |
| 雕刻工作台 | 三栏：左栏按步骤 ①通信链路 ②加工内容（文字/图片/G 码/画板 四个图标切换）③材质与参数；中栏 ④加工监视舱（刀路预览、包络/耗时/行数/SHA 四项统计、进度、“计算轨迹 / 逐行下发（断点续雕）/ 0x18 软件复位”）；右栏加工反馈 + 控制台（终端 / G 码行视图 / 小智网关 三个页签，清空终端、清空视图） |
| 创作画板 | 左侧大画布（Excalidraw，工具条与导出场景/PNG），右侧三张卡：材料与尺寸、设备、预览与发送（含走边框、状态、取消、下载、诊断、作业摘要）；确认发送弹窗保留脱机离线与“已移动材料”勾选 |
| 材料实验室 | 三栏：材料库表格（搜索、新建、导入、导出、套用最近；厚度为胶囊选择；行内编辑）、材料参数（三种模式 + 参数表单 + 保存）、校准矩阵（区间、行列、设备、生成、按功率/速度着色的网格、发送测试） |
| 语音中心 | 同 §9.5.1 |
| 弹窗 | 偏好设置（MCP 地址改为跳转语音中心）、配对认证（6 格配对码）、关闭窗口、材料编辑、确认发送、审批卡、发送覆盖层 |

**C. 视觉规范（曜石设计）**

| 项 | 规范 |
| --- | --- |
| 背景 | **已选定「等离子流体」（2026-10-05 用户确认）**：WebGL 片元着色器多层域扭曲噪声，紫 / 赤红 / 蓝等离子缓慢流动并随鼠标轻微偏移，叠加闪烁星点、暗角与细噪点；不使用网格、扫描线与霓虹线。**已先行接入现有前端** `frontend/src/components/CyberBackground.jsx`（替换原网格/扫描线/罗盘/HUD 背景，组件名与挂载位置不变）。原型中保留流动星河、超空间跃迁、太空舱三种备选仅供对比，不实施 |
| 背景性能 | 着色器以 50% 分辨率渲染后放大；限帧约 40 fps；页面隐藏时暂停；启动 2 秒内帧率低于 28 fps 自动降到 33% 分辨率并减少星点（隐藏期间不计入自检）；WebGL 不可用时退回 CSS 静态渐变；`prefers-reduced-motion` 时只绘制一帧；卸载时释放 WebGL 上下文。新前端外壳完成后再评估是否叠加毛玻璃卡片（毛玻璃在无显卡环境开销大） |
| 面板 | 半透明玻璃卡片（约 70% 不透明 + `backdrop-filter: blur(16px)`），1px 白色 8% 描边，圆角 16px；光标移入时出现紫色聚光并沿边框亮起 |
| 色彩 | 文字 `#fafafa / #b4b4bd / #7a7a85`；唯一强调色激光紫（`#8b7bff`，渐变 `#a78bfa → #7c6cff → #5b8cff`）用于导航选中、步骤编号、选择态、进度；绿色只用于“开始加工 / 批准”；红色只用于急停、拒绝与危险；金色只用于现场确认与提示 |
| 字体 | Geist（标题与界面）+ 思源黑体（中文）+ Geist Mono（数据、代码、计数）；大标题、Logo、统计数字使用白→灰金属渐变 |
| 按钮 | 主按钮白底黑字；开始加工为绿色渐变并带外扩光环；危险为红色渐变；次要按钮深色渐变 + 细描边；所有按钮有悬停上浮与禁用态 |
| 表格 | 参照 Aether CSS 表格样式：加粗大写表头（深色渐变底）、斑马纹、悬停整行变紫、选中行左侧紫色细条、柔和状态胶囊（圆点 + 文字） |
| 动效 | 页面切换淡入上浮；生成轨迹后刀路描边动画；逐行下发时红色激光头沿刀路移动；审批卡边框红/紫/金渐变旋转；开始加工按钮外圈脉冲。`prefers-reduced-motion` 时全部关闭（星场静止） |
| 可访问性 | 所有可操作元素有可见焦点环；文字与背景对比满足 WCAG AA；窄屏单列且无横向滚动 |

**D. 落地方式**

- 色彩、圆角、阴影、字体统一为 `styles.css` 顶部的 CSS 令牌；组件内联样式（如 `Navbar.jsx` 中大量 `style={{…}}`）迁移为类名，避免改版后两套样式并存。
- `CyberBackground.jsx` 使用已选定的 WebGL 等离子流体主背景，Canvas 星点仅作叠加；保留 CSS 无 WebGL 回退、`aria-hidden` 与 `pointer-events:none`，性能和减少动态效果按上表执行。太空舱仅为历史原型备选，不继续作为实施要求。
- 字体经 `frontend/public` 自托管或随 Vite 打包，不依赖外网（桌面端可能离线）。
- 每个页面改完都用功能对照清单逐项核对，并更新 `frontend/tests/visual-baseline/` 截图。

### 9.6 安装与卸载（r8，详见 [安装包与卸载规范](../developer/packaging-install.md)）

用户要求：后期为方便用户下载，应像常见应用一样**可自定义安装目录**，并能**正常卸载**。要点：

| 项 | 规定 |
| --- | --- |
| 安装器 | Tauri 2 NSIS 安装包 `MOSS_<版本>_x64-setup.exe`；不提供 MSI |
| 安装范围与目录 | `installMode: "both"`：用户先选“只为我安装”（默认 `%LOCALAPPDATA%\MOSS`，免管理员）或“为所有用户安装”（默认 `C:\Program Files\MOSS`，需管理员），再**自定义安装目录**（支持中文与空格路径） |
| 快捷方式 | 开始菜单 `MOSS` 文件夹；可选桌面快捷方式；完成页可“立即运行” |
| 数据位置 | 运行数据始终在当前用户 `%APPDATA%\moss`（代码现状，已核实），与安装目录无关；程序目录只放程序文件 |
| 卸载 | 系统“应用”列表、开始菜单与 `uninstall.exe` 均可卸载；复选框“同时删除我的数据”**默认不勾选**；勾选时由卸载钩子删除 `%APPDATA%\moss`（Tauri 自带的“删除应用数据”只针对 `com.moss.laser` 目录，覆盖不到）；**永不删除**用户自备的 `xiaozhi-server` / `mcp-endpoint-server` 目录与导出文件 |
| 运行中保护 | 安装、升级、卸载前检测 `moss.lock`，MOSS 运行中只提示退出，**不强行结束进程**（强杀会跳过急停顺序） |
| 升级 | 覆盖安装，数据不动，数据库启动时自动迁移 |
| 依赖 | WebView2 用 `embedBootstrapper` 自动安装；另可出离线版（`offlineInstaller`） |
| 签名 | 正式版对 exe 与安装包做代码签名；证书不进仓库 |
| 配置读取 | 已修复：除启动目录的 `.env` 外，额外读取运行目录的 `.env`（`%APPDATA%\moss\.env`），优先级 环境变量 > 启动目录 `.env` > 运行目录 `.env`；界面设置 / `settings.json` 继续可用 |
| Android | 继续使用签名 APK，系统安装与卸载；不支持自定义目录 |

### 9.7 导出目录设置（r9，EXP）

用户要求：App 中可以**自行选择导出文件保存到哪个目录**。

**实施状态（2026-10-06）**：EXP-1..5 已实施（壳层 `apps/desktop/src-tauri/src/export.rs`，前端 `saveFile.js` / `ExportSettingsSection.jsx`）；`settings.json` 中以嵌套对象 `"export": { "dir": …, "ask_each_time": … }` 存储；导出成功提示的“打开所在文件夹”由命令 `reveal_exported_file` 实现（只接受已存在的文件）。待用户本机构建与 A-82..A-88 验收。

#### 9.7.1 现状（已核实）

- 前端所有导出统一走 `frontend/src/saveFile.js` 的 `saveFile(blob, filename)`：Tauri 环境调用宿主命令 `save_export_file`，普通浏览器用 `<a download>`。调用方：创作画板的 PNG、场景 JSON、G-code 下载（`DrawPage.jsx`），材料库导出（`MaterialLabPage.jsx`）。
- `apps/desktop/src-tauri/src/lib.rs` 的 `save_export_file` 目录写死为：系统“下载”文件夹 → 退回“文档” → 退回应用本地数据目录下的 `exports`；文件名经 `sanitize_export_name` 清洗，重名由 `unique_export_path` 自动加序号，单文件上限 50 MB。导出成功后提示实际保存路径。
- 没有任何可配置项；`/api/settings` 只处理已知字段（`web_port`、`mcp_endpoint`、`jev_*` 等），未知字段被忽略。

#### 9.7.2 行为规定

| 项 | 规定 |
| --- | --- |
| 入口 | 偏好设置（`SettingsModal`）新增可折叠分区“导出”，按 §9.5.5 视觉实现 |
| 显示 | 当前导出目录的完整路径；未设置时显示“默认：下载文件夹”及解析出的实际路径 |
| 选择目录 | “选择文件夹…”按钮打开**系统文件夹选择框**（由 Rust 侧 `tauri-plugin-dialog` 调起，前端不接收任意路径输入框）；取消选择不改变设置 |
| 恢复默认 | “恢复默认”清除自定义值，回到现有回退顺序（下载 → 文档 → 应用数据 `exports`） |
| 打开文件夹 | “打开文件夹”在资源管理器中打开当前导出目录；导出成功提示中同样提供“打开所在文件夹” |
| 每次询问（可选） | 开关“每次导出时询问保存位置”，默认关闭；开启时每次导出弹出系统“另存为”框，初始目录为当前导出目录、初始文件名为原文件名；用户取消视为放弃导出，提示“已取消导出”，不报错 |
| 生效范围 | 画板 PNG、场景 JSON、G-code 下载、材料库导出等所有经 `saveFile` 的导出；不影响运行数据目录（`%APPDATA%\moss`）、数据库、日志和作业文件的位置 |
| 目录不可用 | 导出时若目录已不存在、不可写（如 U 盘拔出、网络盘断开、权限变更）：**不自动建目录、不静默改存到别处**；返回错误码 `export_dir_unavailable` 并提示“导出目录不可用：<路径>”，提供“重新选择”与“本次保存到下载文件夹”两个按钮 |
| 保存后 | 设置立即生效，无需重启；重启后保持 |

#### 9.7.3 存储与接口（拟新增，仅桌面壳）

- 存储：运行目录 `settings.json` 新增键 `export.dir`（字符串，绝对路径；缺省或空表示默认）与 `export.ask_each_time`（布尔，默认 `false`）。读-改-写时保留文件中的其他键，写入采用临时文件 + 重命名，避免与 `/api/settings` 写 `web_port` 等键时互相覆盖或写出半截文件。
- Tauri 命令（登记到 `capabilities/default.json` 并生成对应 ACL 权限，避免重演 `get_backend_origin` 未登记导致的启动失败）：

| 命令 | 作用 |
| --- | --- |
| `get_export_settings` | 返回 `{ dir, default_dir, effective_dir, ask_each_time, available }` |
| `pick_export_dir` | 调起系统文件夹选择框；选中后校验并保存，返回新的设置；取消返回 `null` |
| `reset_export_dir` | 清除自定义目录 |
| `set_export_ask_each_time` | 设置“每次询问”开关 |
| `open_export_dir` | 用系统文件管理器打开当前生效目录 |
| `save_export_file`（改造） | 按“自定义目录 → 现有回退顺序”确定目录；开启“每次询问”时先弹“另存为”框；新增可选参数 `fallback_default: bool`，仅在用户点了“本次保存到下载文件夹”后由前端传 `true` |

- 校验（选择时与每次导出时都做）：绝对路径；目录存在；可写（在目录中创建并删除一个临时探测文件）。文件名清洗、50 MB 上限、重名加序号保持现有逻辑不变，最终路径必须仍位于所选目录内（防止文件名中的路径分隔符逃逸）。
- **安全边界**：导出目录只能由桌面壳命令经系统选择框修改；`/api/settings` **不接受** `export.dir` / `export.ask_each_time`（保持忽略），局域网浏览器页面与 `moss-host` HTTP 客户端无法改写导出位置，避免把“导出”变成向任意目录写文件的通道。
- 前端：`saveFile.js` 处理 `export_dir_unavailable`（弹出两按钮提示）与“取消导出”；`savedMessage` 增加“打开所在文件夹”动作；`SettingsModal` 新增“导出”分区。

#### 9.7.4 跨端与卸载

| 环境 | 行为 |
| --- | --- |
| Windows 桌面 | 完整支持本节全部功能 |
| Android | 本期不做：系统目录选择返回的是存储访问框架（SAF）的 `content://` 地址，写入方式不同，需单独设计；保持现有行为，偏好设置中不显示“导出”分区 |
| 普通浏览器（`moss-host` 网页） | 不显示该分区；保存位置由浏览器自己的下载设置决定 |
| 卸载 | 与 §9.6 一致：卸载**永不删除**导出目录及其中文件，即使勾选“同时删除我的数据”；`settings.json` 中的 `export.*` 随 `%APPDATA%\moss` 一并处理 |

### 9.8 手机连接电脑 MOSS（r10，LINK）

用户要求：手机与电脑数据一致。对比 Anki（USN 双向同步）、cr-sqlite（CRDT）、Syncthing（冲突副本）与 OctoPrint / Klipper / Moonraker（主机为唯一数据源）后，选择**主机为唯一数据源**：不做双向同步，手机 App 直接调用电脑 MOSS 的接口。理由：无冲突与静默覆盖风险（材料参数错误有安全后果）；复用现有局域网配对与 CORS；工作量最小。代价：电脑须开机且在同一局域网。

#### 9.8.1 行为

| 项 | 规定 |
| --- | --- |
| 电脑端开关 | 偏好设置“手机连接”：“允许手机连接”写入 `settings.json` `lan_access`；重启后 HTTP 监听 `0.0.0.0`（`LASER_WEB_HOST` 环境变量优先）。默认关闭，仍只监听 `127.0.0.1` |
| 地址与配对码 | 同一分区显示本机局域网 IPv4 与实际端口（`GET /api/system/lan-info`，仅本机回环）；“生成配对码”调用既有 `/v1/auth/challenge`（仅回环），5 分钟有效 |
| 手机 App 模式 | 偏好设置“连接电脑 MOSS”：填 `IP[:端口]`（默认 9666），先探测 `GET /api/health` 成功才保存（`localStorage` `moss.remoteHost`）并重载；首个 401 弹出配对窗口，配对后重载。“断开”清除地址与 token 回到本机模式 |
| 启动失败 | 远程模式连不上电脑时启动页标题为“无法连接电脑 MOSS”，提供“重试”与“改用手机本机模式” |
| 手机能做 | 连接雕刻机（由电脑连接）、预览、发送、走边框、安全动作、急停、查看材料与校准、下载 G-code。严格模式开启时 R3 按 §6.2 走桌面审批（`CallerContext::Lan`） |
| 手机只读 | 材料库新建 / 修改 / 删除 / 导入（`/api/material-lab/manage` 除 `export` 外）、校准单元格存为推荐参数（`/api/material-lab/select-cell`）、`/api/settings`、Jev 测试与查看密钥：局域网调用方一律 403 `lan_read_only`。规则在后端 `is_reference_data_write`，局域网浏览器访问同样适用；手机端不显示修改类按钮（套用最近、导入、新建、管理材料、保存到材料库），参数输入框只读，校准单元格不可点选保存，偏好设置隐藏电脑侧设置项；材料、参数与校准结果照常显示，导出库保留 |
| 凭据 | 远程模式**不取也不发送**桌面会话头；配对 token 存 `localStorage`（App 重启保留；电脑重启后 token 失效，401 时重新配对） |
| 图片 | 远程模式下预览图经鉴权 `fetch` 取 blob 显示（`<img>` 直链不带 token） |
| Android 网络 | `network_security_config.xml` 改为允许明文 HTTP（电脑地址不固定，无法按网段配置）；小智接入点仍为 `wss://` |
| 手机本机模式 | 保持不变：手机内置后端独立运行，数据不与电脑互通；临时同步材料用既有“导出库 / 导入” |

#### 9.8.2 不做

双向自动同步、手机离线编辑后合并、云端中转、自动发现电脑（mDNS）——需要时另立工作包评估。

---

## 10. 实施阶段与工作包

逐步执行清单与进度记录见 [实施 TODO](moss-fusion-implementation-todo.md)。每个阶段独立可验收；未完成上一阶段的退出条件不进入下一阶段。所有代码阶段遵守 `AGENTS.md` 的测试、CHANGELOG 与三端构建要求。

推荐顺序：**S1、S2（可与 P0 并行）→ P0 → P2a → P1 → P3 → P4（语音中心）→ P2b（可选）→ P5**；UI-2 前端整体改版在用户确认设计后可独立进行（建议早于 P1/P4，使新页面直接按新规范实现）。P2a 不改 MOSS 代码，先证明自建语音链路可行；P1 完成后同一条接入点链路即可承载经审批的加工。

### S1 CORS 收紧与来源校验（独立于融合，可先行交付）

| 工作包 | 位置 | 内容 |
| --- | --- | --- |
| S1-1 | `crates/laser-api` | 按 §6.5 实现允许来源判定、响应头回显与 `Vary: Origin`、非 GET 来源校验、`OPTIONS` 处理；`Allow-Headers` 加入 `X-Moss-Desktop-Session` |
| S1-2 | `crates/laser-runtime` / `.env` 说明 | 读取 `MOSS_ALLOWED_ORIGINS`，拒绝 `*` |
| S1-3 | 测试 | 更新两处断言 `*` 的测试；新增：允许来源回显、不允许来源 POST（含 `text/plain`）403、无 `Origin` 请求正常、同源局域网页正常、预检 |
| S1-4 | 实测 | Windows 桌面端与 Android 内嵌页的实际 `Origin`；E2E（`npm run test:e2e`）全绿 |
| S1-5 | 文档 | `CHANGELOG.md`（标注破坏性变更）、api-contracts §1/§5、safety §2 局域网访问、quick-start 排障 |

退出条件：A-29 通过；Rust 测试、前端测试、E2E 通过；前端、Windows App、Android APK 重新构建成功。

### S2 顶栏真实状态（独立修复，可先行交付，设计见 §9.5.3）

| 工作包 | 位置 | 内容 |
| --- | --- | --- |
| S2-1 | `crates/laser-api` | `/api/system/info` 去掉编造值：`client_info`、`recent_events`、`last_request` 无数据时为空，`messages_count` 取真实值，`tools_count` 取实际清单长度；新增 `mcp.connected` |
| S2-2 | `crates/laser-contracts` / `laser-mcp` | 遥测单例记录桥的握手状态（连上、断开时更新） |
| S2-3 | `frontend/` | `Navbar` 小智胶囊改为四种状态并显示链路；暂无语音中心时链路固定显示“官方” |
| S2-4 | 测试与文档 | 单测覆盖“已配置未连接时不返回编造值”；更新 api-contracts；CHANGELOG |

退出条件：A-72、A-73 通过；Rust 测试、前端测试、E2E 通过；三端构建成功。

### UI-2 前端整体改版（设计见 §9.5.5；已有定稿/软件实现记录，真机性能仍单独验收）

| 工作包 | 内容 |
| --- | --- |
| UI-2a | 用户确认最终原型版本（`frontend/ui-preview-forge-v2.html` 的某一版），冻结视觉规范 |
| UI-2b | 令牌与基础组件：`styles.css` 令牌、按钮、输入、分段、卡片、表格、胶囊、弹窗；迁移内联样式 |
| UI-2c | 外壳：左侧导航、顶栏（含常驻急停、审批按钮占位）、WebGL 等离子流体背景（含 CSS 回退）、字体本地化 |
| UI-2d | 逐页改版：雕刻工作台 → 创作画板 → 材料实验室；每页完成后按功能对照清单核对 |
| UI-2e | 视觉基线截图、E2E、三端构建；CHANGELOG 记录界面改版（无行为变化） |

退出条件：A-79、A-80 通过；E2E 与前端单测不变通过；三端构建成功。语音中心页与审批卡分别在 P4、P1 中按同一规范实现。

### P0 前置验证（不写 MOSS 代码）

| 工作包 | 内容 | 产出 | 退出条件 |
| --- | --- | --- | --- |
| P0-1 | **云端模型端点验证**（不评估本机硬件）：从将运行 xiaozhi-server 的机器上，用 OpenAI 协议逐项确认 ①`/chat/completions` 支持 `tools` 函数调用与流式；②是否提供 `/audio/transcriptions`（ASR）与 `/audio/speech`（TTS），以及支持的音频格式、`model`、`voice`；③各自首包与整句延迟。若云端没有 ASR/TTS 端点，记录替代方案（上游其他 provider） | `docs/integration/self-hosted-model-bench.md`（只写占位地址，不写密钥） | LLM 函数调用可用；ASR/TTS 各有一条可用路径；整轮延迟在用户可接受范围（建议 <8 s，用户可调整） |
| P0-2 | 锁定版本：xiaozhi-esp32-server 已记录 `87c6df7`（MIT，r5）；补记 `mcp-endpoint-server` 的提交号与许可证；xiaozhi-client 视 P5 决定 | 锁定表 | 许可证允许本地部署 |
| P0-3 | 确认实战派 ESP32-S3 当前固件版本，以及如何修改 OTA 接口地址（配网页配置或重新编译） | 操作记录 | 设备能连到自建服务端并完成一轮纯聊天 |
| P0-4 | **已完成（r5，源码阅读）**：启动命令、配置文件与字段、端口、接入点协议与健康接口、插件接口均已核实（见 §2“上游源码核实”）。剩余：实际运行验证留给 P2a | 本文 §2 | — |
| P0-5 | 决定海门与 xiaozhi-client 是否进入 P5 | 决策记录 | — |

### P2a 接入点零代码验证（只读与预览）

| 工作包 | 内容 |
| --- | --- |
| P2a-1 | 最简化部署 xiaozhi-esp32-server，先用默认/云端模型跑通设备对话（隔离模型变量） |
| P2a-2 | 部署 MCP 接入点；把 MOSS `mcp_endpoint` 指向它；确认 `tools/list` 返回 19 个工具 |
| P2a-3 | 语音测试 R0/R1：连接检查、状态、文字预览、参数推荐、调参建议 |
| P2a-4 | 记录与官方链路的差异（工具描述截断、函数调用格式、超时） |

退出条件：A-01、A-03、A-14 实机通过。**本阶段禁止 `confirm_send` 与任何 R3 动作**（未开严格模式，模型可自行确认）。另需确认：MOSS 最慢的只读/生成工具（文字预览）在 30 秒工具超时内返回。若接入点实测不兼容，记录原因，改为启用 P2b 网关 + 插件作为正式链路。

### P1 MOSS 调用方上下文、网关与审批（可完全用 fake 验证）

| 工作包 | 位置（拟） | 内容 |
| --- | --- | --- |
| P1-0 | `crates/laser-api`、`crates/laser-host` | **纯签名改造**：引入 `CallerContext`（§6.4），`serve`/`handle_http_from`/`handle` 及各 R3 执行入口加 `ctx` 参数；`InProcessBackend` 改用 `handle_http_with_ctx(.., McpBridge)`。不改任何判定，现有测试必须全绿。单独提交 |
| P1-1 | `crates/laser-contracts` | `CallerContext`、`ApprovalRecord`、`ApprovalStatus`、`AuditDetail` 强类型与 JSON Schema（`GatewayCallRequest` 移至 P2b） |
| P1-2 | `crates/laser-runtime` | schema v4：`approvals`（含 `connection` 列）；`audit_log` 写入方法；迁移测试（内存库、重复执行、中断重试、旧版程序打开 v4 库）。`gateway_requests` 表移至 P2b |
| P1-3 | `crates/laser-api` | `ApprovalService`：创建（解析显式目标，否则 `target_required`）、digest 计算、批准、拒绝、过期、单次消费；`/v1/approvals/*` 路由（仅桌面会话） |
| P1-4 | `crates/laser-api` | 严格模式：`require_r3_authority` 接入 `launch_send_job_for`、安全动作 R3、`/api/device/framing`、材料矩阵发送 |
| P1-4b | `crates/laser-api` | 确认 S1 已交付；审批接口纳入同一来源校验 |
| P1-5 | `crates/laser-mcp` | R0–R4 分级函数；严格模式下 MCP 桥（接入点）调用的 R3 返回 `approval_required`，R2 照常执行；分级函数供 P2b 网关复用 |
| P1-6 | `crates/laser-host` / `apps/desktop` / `frontend/` | 严格模式开关组装；`get_desktop_session` Tauri 命令（Android 返回不可用）；`authSession.js` 取会话、`apiFetch` 统一加 `X-Moss-Desktop-Session`（§9.5.4） |
| P1-7 | `frontend/` | 全局审批卡（§9.5.2）：挂在 `App` 根组件，任何页面弹出；现场确认勾选门槛；倒计时；失败原因在卡内显示；批准后接现有 `SendOverlay`；顶栏“审批”按钮与红色角标 |
| P1-8 | `docs/` | 同步 api-contracts、db-schema、safety、architecture、feature-glossary、frontend-behavior |

退出条件：§11 A-20..A-28（A-20 以 MCP 桥调用验证）、A-35、A-36、A-40、A-41 在 `FakeGrbl` 与 E2E 中通过；三端构建成功。完成后，P2a 的接入点链路在严格模式下即可用于经审批的加工（首次实机出光另行授权）。

### P2b 网关与插件（可选增强）

仅在需要以下能力时实施：批准后由小智语音播报执行结果（插件经 `speak_txt` 主动播报）、按会话的审计与幂等、或接入点实测不可用且用户决定采用替代链路。完整技术框架、修改文件、步骤、数据接口、异常安全及测试见 [P2b 实施细则 s1](moss-self-hosted-extension-implementation.md#p2b)。实施时补做 §5 网关接口、`GatewayCallRequest`、`gateway_requests` 表、令牌管理与 A-10..A-13；P2b 不成为 P3/P4 的默认硬前置。

| 工作包 | 内容 |
| --- | --- |
| P2b-1 | 编写 `moss_tool` 插件：工具描述从 `GET /v1/gateway/tools` 动态拉取；处理 `approval_required` / `target_required` 播报与轮询 |
| P2b-2 | （r5：系统提示词随 P2a/P3 编写，见实施清单；此处只补充网关相关话术）编写自建服务端系统提示（可超过 2000 字），覆盖路由建议表（[小智 GRBL 工具边界](../laser/xiaozhi-grbl-tool-boundary.md)）、“必须到电脑确认”话术、“停”只映射 `cancel` / `emergency_stop` |
| P2b-3 | 网关与接入点并存时，回归 A-25，确认两条入口都只能产生审批请求 |

退出条件：A-10..A-13 通过；插件单测（模拟网关）通过；播报流程实机验证需用户授权。

### P3 切换到自有云端模型

在**外部 xiaozhi-server** 中逐个把 ASR → TTS → LLM 切到自有云端端点（`type: openai`，地址与密钥只写在服务端配置，不进仓库），每换一个回归 A-01..A-14。LLM 需验证函数调用格式稳定性（以当时实际暴露给模型的工具列表测选择与参数准确率），不达标时缩小模型可见工具集，执行层权限不变。逐模块修改位置、YAML 字段、失败回滚、假/真实验收见 [P3 六项实施细则 s1](moss-self-hosted-extension-implementation.md#p3)。

### P4 语音中心（设计见 §9.4）

前置：P1 完成；外部服务端启动命令/配置文件经 P0/P2a 核实且接入点已验证。**P2b 是可选网关**，仅当接入点实测不兼容且明确选择替代方案时才要求 P2b 先行验证；不再要求默认跑通 P2b。

| 工作包 | 位置（拟） | 内容 |
| --- | --- | --- |
| P4-1 | `apps/desktop/src-tauri` | 服务端进程管理：启动、停止、重启、外部实例识别、Job Object、退出提示、日志轮转与脱敏 |
| P4-2 | `crates/laser-api` 或 Tauri 命令 | `/v1/voice/*`（仅桌面会话）或等价 Tauri 命令 |
| P4-3 | `crates/laser-runtime` | `settings.json` 新增 `voice.*` 与链路模式；官方 `mcp_endpoint` 备份与恢复 |
| P4-4 | 同上 | 链路模式切换：互斥、先断旧桥再接新桥、联动网关与严格模式 |
| P4-5 | 桌面端 | 模型配置向导：测试连接（内存密钥）、写服务端配置并备份 |
| P4-6 | `frontend/` | `/voice` 语音中心页（§9.5.1）：链路单选、四张状态卡、三栏模型配置、审批记录、脱敏日志；`SettingsModal` 移除 MCP 地址改为跳转说明；Android 隐藏 |
| P4-7 | `docs/` | quick-start 增加语音中心章节；feature-glossary、frontend-behavior、api-contracts 同步 |
| P4-8 | 打包评估 | 只出结论，不实施（§9.4.8） |

退出条件：A-60..A-69 通过；A-50 Android 回归；三端构建成功。

### PKG 安装包与卸载（设计见 §9.6 与 packaging-install.md；可在任意阶段后进行，建议在 UI-2 前端改版后、对外分发前完成）

| 工作包 | 内容 |
| --- | --- |
| PKG-1 | 开启 `bundle.active`，`targets: ["nsis"]`，补全发布者、图标、`installMode: "both"`、中文语言、开始菜单文件夹、WebView2 模式 |
| PKG-2 | 安装版配置读取：已实施读取 `%APPDATA%\moss\.env`（`config::load_env_files`），`migrate` 默认目标对齐运行目录，已记 CHANGELOG；待本机 `cargo test` 验证 |
| PKG-3 | `installer/hooks.nsh`：安装/升级/卸载前检测 `moss.lock`；卸载时按用户选择删除 `%APPDATA%\moss` 并做路径防误删校验 |
| PKG-4 | 安装器横幅、侧栏图片（按 §9.5.5 视觉） |
| PKG-5 | 交付构建改为产出安装包；`AGENTS.md` 增加安装包产物检查 |
| PKG-6 | 代码签名（正式版）与 Android release APK 签名 |
| PKG-7 | 文档：快速开始“安装与卸载”、README 安装说明、架构总览 §6 |

退出条件：packaging-install.md §5 的 PK-01..PK-09 在干净的 Windows 虚拟机上通过（PK-10、PK-11 为正式分发前要求）；三端构建成功。

### EXP 导出目录设置（设计见 §9.7；独立小功能，可在任意阶段先行交付）

| 工作包 | 位置 | 内容 |
| --- | --- | --- |
| EXP-1 | `apps/desktop/src-tauri` | 加入 `tauri-plugin-dialog`（仅桌面目标）；新增 `get_export_settings`、`pick_export_dir`、`reset_export_dir`、`set_export_ask_each_time`、`open_export_dir`；改造 `save_export_file`（自定义目录、每次询问、`export_dir_unavailable`、`fallback_default`）；登记 capability / ACL |
| EXP-2 | 同上 | `settings.json` 中 `export.*` 的读写（保留其他键、临时文件 + 重命名）；目录校验与路径逃逸检查抽成可单测的纯函数 |
| EXP-3 | `frontend/` | `SettingsModal` 新增“导出”可折叠分区；`saveFile.js` 处理不可用目录与取消；成功提示加“打开所在文件夹”；非 Tauri 与 Android 不显示分区 |
| EXP-4 | 测试 | Rust：目录解析顺序、不可用目录不回退、路径逃逸被拒、`settings.json` 其他键保留；`/api/settings` 提交 `export.dir` 不生效；前端：`saveFile` 各分支（含取消与不可用）、分区显示条件 |
| EXP-5 | 文档 | `CHANGELOG.md`；[前端行为契约](../developer/frontend-behavior.md)偏好设置“导出”分区；[快速开始](../user/quick-start.md)“导出文件保存在哪里”；[安装包与卸载规范](../developer/packaging-install.md)确认卸载不删导出目录 |

退出条件：A-82..A-88 通过；Rust 测试、前端测试、E2E 通过；前端、Windows App、Android APK 重新构建成功（Android 确认行为无变化）。

### LINK 手机连接电脑 MOSS（设计见 §9.8；独立功能）

| 工作包 | 位置 | 内容 |
| --- | --- | --- |
| LINK-1 | `laser-runtime` | `web_host_from`：`LASER_WEB_HOST` > `lan_access` > `127.0.0.1` |
| LINK-2 | `laser-api` | `is_reference_data_write` + `lan_read_only` 守卫；`/api/settings` 接受 `lan_access`；`GET /api/system/lan-info`（仅回环） |
| LINK-3 | `frontend` | `remoteHost.js`；`authSession` 远程模式（不发桌面会话、token 存 `localStorage`）；`PhoneLinkSection`；启动失败页；配对后重载；材料页只读提示；预览图 blob |
| LINK-4 | Android | `network_security_config.xml` 允许明文 |
| LINK-5 | 文档 | CHANGELOG、quick-start §7 |

退出条件：A-89..A-96 通过；Rust 测试、前端测试、E2E 通过；三端构建成功。

### P5 可选扩展（未决定实施；[六项实施细则 s1](moss-self-hosted-extension-implementation.md#p5)）

- xiaozhi-client 可作为额外 MCP 聚合层，但须先对比已实现 NV9，确认有独立价值；不自动要求启用网关，MOSS 原执行层 R3/R4 规则不变。
- 海门 Agent 仅可作为工具提供方，不监听设备音频（D1），不得继承激光控制权限；需要核实真实上游协议与沙箱限制后由用户决定是否实施。

---

## 11. 验收矩阵

层级：U=单元测试，I=集成（FakeGrbl/回环模拟器），E=E2E，M=实机（需另行授权，结果未执行前一律 `unverified`）。

| ID | 场景 | 层级 | 期望 |
| --- | --- | --- | --- |
| A-01 | 设备连接自建服务端并唤醒 | M | 屏显在线，可对话 |
| A-02 | 自有云端 ASR 识别中文指令 | M | 文本正确率满足 P0-1 设定 |
| A-03 | 自有云端 TTS 播报 | M | 可听清，无截断 |
| A-04 | 整轮延迟 | M | 满足 P0-1 预算 |
| A-10 | 网关关闭时调用 | U/I | 404 `gateway_disabled` |
| A-11 | 无令牌 / 错误令牌 / 配对 token | U/I | 401 `gateway_unauthorized` |
| A-12 | R0/R1 调用（status、preview） | I | 结果与直接 MCP 调用一致 |
| A-13 | 同 `request_id` 重复提交 | U/I | 只执行一次，返回首次结果 |
| A-14 | P2a：经自建接入点语音调用 R0/R1 | M | `tools/list` 19 个；预览生成成功；未触发任何发送 |
| A-20 | 模型传 `confirmed=true` 调 `confirm_send` | I | 不发送，返回 `approval_required`，审计记录删除动作 |
| A-21 | 桌面批准后执行 | I/E | 使用审批记录中的连接参数进入现有门禁，FakeGrbl 收到正确字节流 |
| A-22 | 批准后重新预览再执行 | I | `approval_mismatch`，不发送 |
| A-23 | 授权过期 / 重复使用 | U/I | `approval_expired` / `approval_consumed` |
| A-24 | 严格模式下回环 HTTP（无桌面会话头）直接 `confirm_send` | I | 拒绝 `strict_mode_blocked` |
| A-25 | 严格模式下旧 MCP 桥 / 接入点 R3 | I | `approval_required` |
| A-26 | 严格模式下 `/api/device/framing`、安全动作 `home` | I | 无授权拒绝 |
| A-27 | R2：`cancel`、`emergency_stop` 经网关 | I | 无审批直接执行；`0x18` 经在途作业传输写入，不排在 G-code 队列之后 |
| A-28 | R4 工具 | U | `tool_not_allowed` |
| A-29 | 默认模式（未开严格模式）下带不允许 `Origin` 的 POST（含 `text/plain` 简单请求） | U/I/E | 403 `origin_not_allowed`，不进入路由；允许来源回显且带 `Vary: Origin`；同源局域网页、无 `Origin` 客户端正常；预检含 `X-Moss-Desktop-Session` |
| A-30 | 模型服务宕机 | M | 设备播报错误；MOSS 网页可正常预览/发送 |
| A-31 | 服务端宕机期间待批准请求 | I | 到期 `expired` |
| A-32 | 批准后、设备阶段前 MOSS 崩溃 | I | 重启后 `expired`，workflow 仍可重新批准 |
| A-33 | 发送中 MOSS 崩溃 | I | workflow `failed`，审批 `outcome` 为结果未知，不重放 |
| A-34 | 审计写入失败 | U | R3 拒绝；R2 仍执行 |
| A-35 | R3 审批时串口留空且无 `.env` 默认值 | U/I | `target_required`，不自动选择串口 |
| A-36 | 请求体伪造身份字段 | U | 入口删除，`CallerContext` 不受影响 |
| A-40 | 关闭网关与严格模式 | I/E | 现有全部测试不变通过 |
| A-41 | v4 迁移：重复执行、中断重试；旧版迁移逻辑打开 v4 库 | U | 幂等、可恢复；旧逻辑正常打开并忽略新表 |
| A-42 | 设备切回官方服务端 | M | 原链路可用 |
| A-50 | Android APK 构建与现有 E2E | E | 构建成功，行为无变化 |
| A-60 | 链路模式切换 | I | 官方与自建互斥；切换后只有一条 MCP 链路在线；切回官方时 `mcp_endpoint` 恢复备份值 |
| A-61 | 切到自建模式 | I | 网关与严格模式同时开启；任一开启失败则回滚到切换前状态 |
| A-62 | 启动与停止服务端（用假子进程） | I | 只能停止 MOSS 启动的进程；重复启动被拒绝 |
| A-63 | 外部已运行的服务端 | I | 标为“外部运行”，无停止按钮，停止接口返回 409 |
| A-64 | MOSS 正常退出与异常退出 | I/M | 正常退出时有提示；异常退出后无孤儿进程（Job Object）；急停顺序不变 |
| A-65 | 服务端崩溃 | I | 显示退出码与日志尾部，不自动重启 |
| A-66 | 日志脱敏 | U | 含 `api_key`、`Authorization`、`token` 的行被替换为 `***` |
| A-67 | 配置向导测试与保存 | U/I | 测试不落盘；保存只写服务端配置并生成 `.bak`；MOSS 的 SQLite、`settings.json`、日志中搜索不到密钥 |
| A-68 | `/v1/voice/*` 鉴权 | U | 非桌面会话一律 403；`moss-host serve` 中 404 |
| A-69 | Android 构建 | E | 不含进程管理代码，无语音中心入口 |
| A-70 | 审批卡全局弹出 | E | 在雕刻工作台、材料实验室、创作画板、语音中心任一页面，出现待批准请求时都会弹出 |
| A-71 | 审批卡门槛与失败显示 | U/E | 未勾选现场确认时“批准”不可点；`approval_mismatch` / `approval_expired` 在卡内显示原因且不发送；批准后显示 `SendOverlay` 四步且急停始终可见 |
| A-72 | `/api/system/info` 不再编造 | U | 已配置但未连接时：`connected=false`，`client_info`、`recent_events`、`last_request` 为空，`messages_count=0`；`tools_count` 与实际清单一致（19 / 20） |
| A-73 | 顶栏小智四种状态 | U/E | 未配置 / 未连接 / 等待设备 / 已连接四种判定与样式正确，并显示官方或自建 |
| A-74 | 顶栏审批按钮 | E | 无请求为灰；有请求变红并显示数量；点击打开审批卡 |
| A-75 | 桌面会话头 | U | Tauri 环境下 `apiFetch` 每个请求都带 `X-Moss-Desktop-Session`；非 Tauri 环境不带；会话不写入 `localStorage` |
| A-76 | 会话失效恢复 | U | 403 时重新取会话并只重试一次；仍失败给出明确提示 |
| A-77 | MCP 地址唯一入口 | E | `SettingsModal` 不再提供 MCP 地址输入；语音中心可修改；`/api/settings` 旧字段仍兼容 |
| A-78 | 语音中心页 | E | 链路切换确认框、状态卡、三栏模型“先测试后保存”、审批记录、日志脱敏均按 §9.5.1 工作（后端用假数据） |
| A-79 | 前端改版视觉 | 人工 | 四个页面与全部弹窗在桌面、移动视口截图核对符合 §9.5.5 C；`prefers-reduced-motion` 下无动画、星场静止；无横向滚动 |
| A-81 | 安装与卸载 | 人工 | 按 packaging-install.md §5 PK-01..PK-11 执行（干净 Windows 虚拟机：自定义目录含中文空格、两种安装范围、快捷方式、默认卸载保留数据、勾选删除数据、运行中保护、覆盖升级） |
| A-80 | 改版功能不变 | E/人工 | 功能对照清单 83 项逐项可找到且行为与改版前一致；现有 E2E（`npm run test:e2e`）不修改断言即通过 |
| A-82 | 选择导出目录 | 人工/E | 偏好设置“导出”分区显示当前目录；“选择文件夹…”选中含中文与空格的目录后立即生效，画板 PNG、场景 JSON、G-code、材料库导出均保存到该目录，提示中的路径一致；重启后设置保持 |
| A-83 | 恢复默认与打开文件夹 | 人工 | “恢复默认”后回到下载文件夹；“打开文件夹”与成功提示中的“打开所在文件夹”打开正确目录 |
| A-84 | 目录不可用 | U/人工 | 删除或拔出所选目录后导出：返回 `export_dir_unavailable`，不自动建目录、不写到别处；“本次保存到下载文件夹”只影响这一次 |
| A-85 | 每次询问 | 人工 | 开启后每次导出弹“另存为”框且初始目录正确；取消时提示“已取消导出”、不产生文件 |
| A-86 | 安全边界 | U | `/api/settings` 提交 `export.dir` 不改变导出目录；文件名含 `..\`、`/` 等时最终路径仍在所选目录内；`settings.json` 其他键不丢失 |
| A-87 | 跨端 | E/人工 | 普通浏览器与 Android 不显示“导出”分区，导出行为与改动前一致 |
| A-88 | 卸载保留导出 | 人工 | 勾选“同时删除我的数据”卸载后，自定义导出目录及其中文件仍在 |
| A-89 | 电脑开启手机连接 | 人工 | 勾选并重启后监听 `0.0.0.0`；分区显示局域网地址与端口；未勾选时只监听 `127.0.0.1` |
| A-90 | 手机连接与配对 | M | 手机填地址连接成功，弹出配对窗口；输入电脑生成的配对码后各页面加载电脑数据；地址错误或电脑未开时给出明确原因 |
| A-91 | 手机操作雕刻机 | M | 手机上经电脑连接 FluidNC、预览、发送、急停均可用；急停 `0x18` 不排在 G-code 之后 |
| A-92 | 手机只读 | U/M | 局域网 token 调用材料保存 / 删除 / 导入、`/api/settings`、`/api/jev/reveal` 均 403 `lan_read_only`；导出与雕刻机操作不受影响；电脑本机不受影响 |
| A-93 | 不发送桌面会话 | U | 远程模式下请求不带 `X-Moss-Desktop-Session`，且不调用 `get_desktop_session` |
| A-94 | 电脑重启 | M | 手机下一次请求 401 → 弹出配对窗口；重新配对后恢复 |
| A-95 | 断开与回退 | M | “断开”或启动页“改用手机本机模式”后回到手机内置后端，数据为手机本地数据 |
| A-96 | 预览图 | M | 远程模式下工作台与画板预览图正常显示 |---

## 12. 风险与未决项

| 风险 / 未决 | 影响 | 处理 |
| --- | --- | --- |
| 接入点工具调用 30 秒超时 | 慢工具（文字预览、图像生成）超时，模型以为失败 | P2a 实测最慢工具；超时风险高的工具改为先返回“已开始生成”，再用 `status` 查询 |
| ASR 非流式上传 | 长句识别慢 | P0-1 测延迟；必要时换上游流式 ASR provider |
| P2b 依赖上游内部函数（`speak_txt`） | 上游升级后插件失效 | 锁定版本；P2b 为可选 |
| P1-0 签名改造面大（`laser-api/src/lib.rs` 约 290 KB） | 工期与回归风险 | 纯签名改造单独提交并要求现有测试全绿，再叠加逻辑 |
| 现有 CORS `*` + 回环免鉴权 | 本机浏览器网页可跨站调用回环接口（现状即存在） | 用户已决定修复：S1 默认始终生效（§6.5），按破坏性变更记录 |
| P2a 期间模型可自行确认 | 误触发加工 | P2a 明确禁止 R3；提示词不提供发送路径；不连真实设备或保持断电亦可 |
| 自有 LLM 函数调用不稳定 | 选错工具、参数缺失 | 缩小工具集；网关参数校验；R3 必经人工审批兜底 |
| 云端模型网络延迟或不可用 | 对话卡顿、无响应 | P0-1 实测；xiaozhi-server 配置超时；MOSS 不依赖模型可用性（G4） |
| 模型密钥泄露 | 云端费用与滥用 | 密钥只写 xiaozhi-server 配置，不进 MOSS 仓库、不进提示词归档；定期轮换 |
| 云端无 ASR/TTS 端点或格式不兼容 | 语音链路缺环 | P0-1 确认；不兼容时改用上游其他 provider |
| 上游接口变化 | 插件失效 | 锁定提交号；插件只依赖网关 HTTP |
| Docker 部署导致非回环访问 | 接口暴露到局域网 | 首选同机源码部署；否则防火墙限制（§9.1） |
| Tauri 实际来源字符串 | 白名单错误导致桌面端或 Android 页面不可用 | S1-4 实测 Windows 与 Android 来源后写死，并提供 `MOSS_ALLOWED_ORIGINS` 排障 |
| 小智固件 OTA 地址修改方式 | 无法切换 | P0-3 |
| 用户离开电脑无法批准 | 语音无法启动加工 | 设计如此；可后续评估“局域网手机批准”作为独立范围（需独立威胁建模） |
| 海门/xiaozhi-client 价值不明 | 范围膨胀 | 放入 P5，P0-5 决策 |
| 语音中心管理外部进程 | 误关用户的服务端、留下孤儿进程 | 只管自己启动的进程；Job Object；外部实例只读 |
| 配置向导接触密钥 | 密钥落入 MOSS 存储或日志 | 只在内存使用；写入目标只有服务端配置；A-67 搜索验证 |
| 设备 OTA 与所选模式不一致 | 用户以为已切换，设备仍连旧服务端 | 页面持续提示一致性；无法判断时显示“未知” |
| 上游配置文件格式变化 | 向导写坏配置 | 写前备份；锁定上游版本；写入后校验可解析 |
| 导出目录被设成系统或程序目录 | 写入失败或文件散落到不合适的位置 | 只能经系统选择框设定；可写探测失败即拒绝；`/api/settings` 不可修改；不自动回退到别处，避免用户找不到文件 |

---

## 13. 文档与提示词同步清单（实施时）

- [Web API 契约](../developer/api-contracts.md)：新增 §`/v1/gateway/*`、`/v1/approvals/*`、错误码。
- [数据库结构](../developer/db-schema.md)：schema v4、`audit_log` 写入方。
- [开发者安全入口](../developer/safety.md)：严格模式、R0–R4 分级、`CallerContext` 身份模型、CORS/来源校验。
- [架构总览](../developer/architecture.md)：网关与审批组件。
- [前端行为契约](../developer/frontend-behavior.md)：新增 `/voice` 路由、全局审批卡、顶栏四种小智状态、`X-Moss-Desktop-Session` 请求头、MCP 地址唯一入口（§9.5）。
- [功能字典](../developer/feature-glossary.md)、[前端行为契约](../developer/frontend-behavior.md)：审批卡、设置页。
- [提示词归档](xiaozhi-prompts/README.md)：自建服务端提示词快照与适用范围。
- [快速开始](../user/quick-start.md)：自建链路启动顺序与回退步骤。
- [快速开始](../user/quick-start.md)：语音中心使用说明、模式切换与设备 OTA 的对应关系（P4）。
- [安装包与卸载规范](../developer/packaging-install.md)、[快速开始](../user/quick-start.md)“安装与卸载”、根目录 README 安装说明（PKG）。
- [前端行为契约](../developer/frontend-behavior.md)、[快速开始](../user/quick-start.md)：偏好设置“导出”分区与“导出文件保存在哪里”（EXP，§9.7）。
- [快速开始](../user/quick-start.md) §7：手机连接电脑 MOSS（LINK，§9.8）。
- `CHANGELOG.md`：每个代码阶段按 `AGENTS.md` 记录。
