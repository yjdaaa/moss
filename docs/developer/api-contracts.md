# Web API 接口契约（`/api/*` 与 `/v1/*` 路由、文件与静态服务）

状态：2026-09-22 按 `crates/laser-api/src/lib.rs` 现状核对。路由表与字段以代码为准；黄金样本 `tests/golden/api/` 记录的是 Python 版响应，比对规则见 §6。
历史依据：原稿从 Python 版 `web_server.py` 提取（2026-09-14）。
用途：前端、MCP 工具转发（`laser_host::InProcessBackend` 直接调用同一 `ApiService::handle_http`）与测试共同遵守的 HTTP 契约。MCP 工具描述和小智提示词的自然语言调整不改变本文约定的字段、状态码和响应结构。

路由总表：

| 方法 | 路径 | 说明 | 详见 |
|---|---|---|---|
| GET | `/api/health` | 存活检查 | §2 |
| GET | `/api/system/info` | 端口、MCP 桥状态与遥测；非回环也公开 | §2 |
| GET | `/api/jobs/stop-targets` | 顶栏急停的后端原始任务目标：`result.jobs` 数组，包含 `job_id`、`target`、`state` (`running` / `unknown`)；状态未知不代表已停机，多个目标不得随机选择 | §2 |
| GET | `/api/ui/runtime-config` | 连接默认值与上传后缀 | §2 |
| GET | `/api/ui/serial-ports` | 系统串口列表 | §2 |
| POST | `/api/ui/discover-controllers` | mDNS 发现 FluidNC（2026-10-13 起由 GET 改为 POST） | §2 |
| GET | `/api/ui/material-options` | 材料名与厚度 | §2 |
| GET | `/api/ui/material-recommendation` | 材料推荐参数 | §2 |
| GET | `/api/material-lab/export` | 材料库导出 | §2 |
| GET / POST | `/api/material-lab/recent` | 最近一条含完整参数的 workflow | §3.11 |
| GET | `/preview` | 受控文件服务 | §2.1 |
| GET | `/`、`/draw`、`/material-lab`、`/assets/*` | 嵌入的前端 | §2.2 |
| POST | `/api/upload` | 上传图片或 G-code | §3.1 |
| POST | `/api/draw/preview` | 画板预览 | §3.2 |
| POST | `/api/connection/check` | 连接检查与设备能力 | §3.3 |
| POST | `/api/text-task/generate` | 文字任务预览 | §3.4 |
| POST | `/api/text-task/send` | 文字任务确认发送 | §3.5 |
| POST | `/api/workflow` | workflow 状态机 | §3.6 |
| POST | `/api/material-lab/preview` / `send` / `status` / `select-cell` / `manage` | 材料实验室 | §3.7～3.10 |
| POST | `/api/device/framing` | 走边框 | §3.12 |
| POST | `/api/safe-action` | 受控安全动作 | §3.13 |
| POST | `/api/settings` | 保存监听端口 | §3.14 |
| GET | `/v1/capabilities`、`/v1/events`、`/v1/material-lab/bootstrap` | 能力、事件、材料页首屏 | §5 |
| POST | `/v1/auth/challenge`、`/v1/auth/pair` | 局域网配对 | §5 |

---

### NV10 内置语音记忆（Tauri 桌面命令，非 HTTP 路由）

记忆摘要仅供 MOSS 桌面主窗口本地 UI 使用。四个命令列入 `build.rs` 并在 `capabilities/voice-memory.json` 中单独授权给 `main`，**不**继承 `default.json` 的 localhost 远程页面权限，Android 不提供真实实现。`voice.builtin_memory_enabled` 缺省为 `false`。

- `voice_memory_list()` → `[{device_id,summary,revision,updated_at}]`，只列已有记忆，不返回原始录音与对话。
- `voice_memory_set_enabled({enabled:boolean})` → `{enabled}`，显式设置开启/关闭；关闭后未完成的后台总结结果不写入。
- `voice_memory_save({deviceId,expectedRevision,summary})` → 最新记忆记录；仅允许设备、最长 1500 Unicode 字符。版本不一致返回 `memory_conflict`（409），不能自动覆盖。
- `voice_memory_clear({deviceId,expectedRevision})` → 最新记录（空摘要，revision+1）；版本冲突同上，用户须刷新。

摘要生成采用已配置的大模型，可为第三方云模型，用户须主动开启并在界面获知数据将发送给其已配置的服务商。服务端仅把已提交的用户/助手文字（筛除工具消息，最多 6000 字符）交给当前 LLM，无工具权限；模型输出按 1500 字符上限和 revision 比对持久化。编辑/清空/关闭/设备撤销通过同一内存协调门禁，原始音频不入记忆数据库。真实模型隐私边界、总结质量和物理设备体验另行验收。

### NV11 内置语音信息插件（Tauri 桌面命令，非 HTTP 路由）

`voice.builtin_plugins` 为不含密钥的配置对象，天气、农历、新闻、联网搜索与RAGFlow五项均默认关闭。秘密保存在SQLite `app_secrets` 的`voice.plugin.weather/search/metaso/rag`；仅packaged `voice-private`窗口通过`capabilities/voice-plugins.json`管理，主窗口/远程origin不能读取或修改。旧配置默认Tavily，`search_provider=tavily|metaso`选择独立host与密钥，失败不换供应商。

- `voice_plugins_status()` → `{settings,keys:{weather:boolean,search:boolean,metaso:boolean,rag:boolean}}`；不返回原始key。
- `voice_plugins_save({input:{settings,keys}})` → 同上。`keys.<kind>` 格式 `{action:"keep"|"set"|"clear",value?:string}`；默认保留，启用需要密钥的插件时须已有或本次新设密钥；配置校验失败时不得使插件生效。新闻源记录 `{id,url,kind?:"rss"|"newsnow",source_id?:string}`，旧记录缺省 `rss`；RSS 只允许 HTTPS，NewsNow 还允许显式填写的本机/LAN HTTP；两者禁止在 URL 中携带 query 或凭据。NewsNow 只访问固定 `/api/s?id=<配置来源ID>`，校验 `status=success/cache`、`id`、`items`，缺字段返回 schema 错误。RAGFlow 数据集 ID 只能由桌面保存，不能从模型输入指定。
- 内置工具名为`get_weather`、`get_lunar`、`get_news`（RSS/NewsNow）、`web_search`（Tavily/Metaso）、`search_rag`（RAGFlow）。调用前后复核voice许可、配置和凭据快照；旧列表不能绕过关闭/撤销。模型不能指定host、坐标或数据集，RAG响应也必须带白名单内dataset_id和非空document_id。原急停`call_raw`完整透传。Metaso固定官方SDK普通搜索契约，独立密钥、非流式，仅安全公开引用，不抓链接或上传文件。
- 成功回`{ok:true,source,observed_at,data,truncated}`；失败回`{ok:false,error:{code,message,retryable}}`，不含原始错误正文/凭据。连接3秒、总计10秒、禁止跳转和自动重试、响应≤1MiB，JSON转义后外层工具文字≤2000字符。401/403鉴权失败、429限流、432/433额度耗尽`plugin_quota_exceeded`且不可重试；超时/坏schema分别固定码。天气/新闻最多32成功条目、5分钟内存缓存，键隔离配置/凭据/参数且同键去重；Tavily/Metaso/RAG不缓存。六家本机TLS合成契约与React假桥已验证，真实账号/限额/数据质量/真人设备仍unverified，见[NV11样本依据](../../crates/moss-voice-server/tests/fixtures/nv11/README.md)。

### NV12 视觉图片 HTTP 接口（内置语音 8003，默认禁用）

只有独立视觉设置启用、模型地址与密钥齐备时，才允许已授权设备通过 MCP initialize 收到 capabilities.vision={url,token}；地址使用实际监听端口。默认不启用电脑或设备摄像头；是否主动采集图片完全由设备自身固件负责。不会把图像写入对话历史、文件或日志。

HTTP POST /mcp/vision/explain，Header 必须包含 Authorization: Bearer <设备 token>、Device-Id、Client-Id；multipart/form-data 仅允许 question（UTF-8，1..2000字）和一个 image（PNG/JPEG）字段，顺序任意但不能重复。最多 5MiB 图片，body 最多 5MiB+64KiB；MIME/魔数匹配，实际解码尺寸最大 4096×4096、总像素≤1600万。先在 middleware 验签，再由后台只读 DeviceRegistry.authorize_http 校验当前允许状态、匹配 client 和授权代次，不调用会改 last_seen 的 seen；返回前按同一代次复核，撤销再允许也不复活在途请求。全局并发2、同设备1、满额不排队；模型最长30秒，响应最大1MiB，文本最多2000字，不重定向。

成功：HTTP 200，JSON {success:true,text}。失败：{success:false,error:{code,message}}；401 无效 token，403 拒绝/已撤销/关闭，413 超大，415 格式不匹配，422 其他图片/请求错误，429 已满（Retry-After:1），502 上游异常，504 超时。所有响应 Cache-Control:no-store。视觉模型结果属于不可信资料，不赋予任何设备控制权限。

桌面仅本地 main 窗口具有 voice_vision_status() / voice_vision_save({input:{settings:{enabled,url,model},key:{action:keep|set|clear,value?}}}) / voice_vision_test() 权限。状态只返回 settings、key_configured、ready，不返回原始密钥；保存默认不开启视觉。测试按钮只使用程序在内存生成的测试 PNG 向已配置模型发起请求，可能产生费用，但不会使用摄像头。Android 命令仅返回不支持；真实固件、真实模型与实机延迟留 FINAL 验收。

### NV13 Claude Code / Codex 专用 AgentRun（部分完成，CLI 执行锁定）

AgentRun 是独立 ApprovalKind，和原有 ExternalTool、激光动作分开。语音调用只有已授权设备及有效 voice session 可以准备任务；模型不能通过参数指定 executable、工作目录、shell、凭据、MCP 配置或者 confirmed。任务为最多 4000 字符的 task，全部经 stdin 传递，固定只读 argv，不由 cmd/PowerShell 解释。

审批摘要绑定 provider、workspace、executable、版本、argv/配置版本和任务；批准时按当前设置重新计算，不一致拒绝。审批卡的 agent 节点只展示 task、provider、工作目录名、版本、read_only_plan 和费用提醒，不返回目录全路径、密钥或内部 request。ApprovalService.consume_agent_run 是专用事务，验证 voice permit、数据库设备 allowed、摘要、期限和批准状态后单次消费，连同 run 记录和审计原子写入；另一条路径不能把 AgentRun 当普通 ExternalTool 或激光动作执行。

schema v8 的 voice_agent_runs 最近 50 条记录包含 run id、approval id、提供者、工作目录（桌面状态仅目录名）、摘要指纹、状态、最多 256 KiB 输出、时间和短错误码；启动后 running -> result_unknown，不能自动重新执行。

桌面 Tauri voice_agent_status() 返回 settings、sandbox_verified=false、reason=agent_sandbox_unavailable、runs（最多50）；voice_agent_save({input:{enabled:false,workspace,codex_executable,claude_executable}}) 只保存本机已有路径并验证敏感目录和明确 exe。命令只经 capabilities/voice-agent.json 授权 main 主窗口（remote 回环，命令内固定本进程后端地址）；Android 不支持。enabled:true 一律拒绝。生产 REAL_CLI_SANDBOX_VERIFIED=false、未注册生产真实 CLI 子进程 runner。配置目录、执行参数和真实进程负向安全测试仍待实现/完成后再授权；不能把 help 单测当作实际沙箱，通过前不会执行 Claude Code/Codex。
### NV14 本地音乐（桌面 Tauri 命令，非公共 HTTP）

`voice.builtin_music` 为默认关闭的配置 `{enabled:false,directory:""}`；现有用户升级后不会自动扫描任何目录。所有命令仅由桌面主窗口的 `capabilities/voice-music.json` 授权（remote 回环，命令内固定本进程后端地址）；普通远程 Web 和 Android 没有真实实现。

- `voice_music_status()` → `{settings:{enabled,directory},running,requires_chat:true}`，纯读配置不扫描。
- `voice_music_pick_directory()` → `{directory:string}`，用户取消为 `null`；用户明确点击后弹出原生文件夹对话框，此时不保存也不扫描。
- `voice_music_save({input:{settings:{enabled:boolean,directory:string}}})` → 设置及 `restarted`、`track_count`；只有 enabled=true 且提供本机目录时建立只读索引；启用状态修改会重启当前内置语音服务、结束旧会话。

模型工具 `play_music({query?:string,random?:boolean})` 只能按歌名或随机二选一选择索引中的曲目。歌名限制为 1..200 字符；精确匹配优先，歧义返回最多5个候选，不自动播放。受控会话收到 `PlayMusic{track_id,title}` 后从受信索引按句柄/size/mtime 重查文件，只在当前有效许可下把 Symphonia 解码的24kHz单声道 PCM 交给已有60ms Opus泵。模型不能指定路径、目录、任意 URL、外部播放器或在同轮同时调用其他工具；歌曲不写入对话历史。切歌、唤醒、abort、自然插话、设备撤销或断线均取消当前源与旧 generation。假小智与合成 WAV 已通过软件测试；真实 MP3/FLAC/Ogg 文件及实体听音未验收。

## 1. 通用约定

### Rust 发送状态增补（2026-09-19）

- 完整 workflow/材料矩阵开始发送前，完整 `$$` 响应、`$32=1`、有限正数的 `$30` 和 Idle（含 `Pn:` 无 `E`/`F`/`D`、无 SD 运行标记）必须通过；`$30` 不是 1000 时按 `$30/1000` 在发送流水线中换算 S 值，原文件与 SHA 门禁不变。
  握手失败返回 400 `device_not_ready`，功率/状态检查失败返回 400 `device_preflight_failed`；不写加工指令。
- 活跃 `send_result` 增加 `phase`：`streaming` 或 `awaiting_completion`；兼容已有 `sending`/`running` 任务状态。
- `lines_sent` 仅表示原文件收到 ACK 的行数。所有行收到 ACK 后保持 `running=true` 和设备占用，
  直到 `G4 P0` 同步 ACK 及后续 Idle 通过，才返回 `completed`（`phase=completed`）。
- 失败/取消沿用既有状态，失败文案可能表示执行结果未知；客户端不能把传输 100% 或失败当成物理停机。

### 1.1 响应包装

| 情况 | 响应体 | HTTP 状态 |
|---|---|---|
| 成功 | `{"success": true, "result": <任意>}` | 200 |
| 失败 | `{"success": false, "result": "<错误文案>"}`，有细节时加 `"detail": <任意>` | 400 / 401 / 403 / 404 / 429 / 500；401、429 仅用于新增鉴权层，403 见 §1.6 与 §5 |

所有 JSON 响应带 `Cache-Control: no-store`。工具层返回的 `success/result/detail` 结构原样透传。

### 1.2 请求体

- 只接受 JSON 对象；`Content-Length` 为 0 视为空对象。
- 超限 → 400 `请求体过大`；解析失败 → 400 `JSON 解析失败: …`；顶层非对象 → 400 `JSON 顶层必须是对象`。
- 上限：`/api/upload` 20 MiB；`/api/draw/preview` 10 MiB；其余 64 KiB。
- 未知路径 → 404 `未找到路径`。

### 1.3 字段转换规则

| 规则 | 行为 |
|---|---|
| bool | 原生 bool 原样；`None`/`""` 取默认；否则转小写去空格后属于 `1 true yes y on 确认 confirmed` 为 true，**其余任何值都是 false** |
| float | `None`/`""` 取默认；`float()` 失败 → 400 `<key> 必须是数字` |
| int | `None`/`""` 取默认；`int(float())`，`"3.7"` → 3；失败 → 400 `<key> 必须是整数` |
| 可选正数 | 空 → 未设置；负数 → 400 `<key> 不能小于 0`；0 → 未设置 |
| 必填文本 | `strip()` 后为空 → 400 `<key> 不能为空` |

Rust 实现复刻这些规则（`parse_bool` / `parse_float` / `parse_int`），包括 `"false"` 字符串被当作 false 只是因为它不在真值集合里。

### 1.4 `result.web` 链接补充

成功时给 `result` 追加 `web` 对象（由 `result.image_file` / `result.gcode_file` 或 `result.artifacts.*` 推导）：

| 字段 | 来源 |
|---|---|
| material_preview_url | `image_file` → `/preview?path=` |
| gcode_preview_url | G-code 渲染的 PNG 预览 |
| gcode_download_url | `/preview?path=<gcode>&download=1` |
| image_preview_url | 等于 material_preview_url，没有则 gcode_preview_url |
| uploaded_image_url、scene_download_url | 仅 `/api/draw/preview`：上传的画板图片与 `.excalidraw.json` 场景 |

没有 `gcode_download_name`、`task_json_url`、`workflow_json_url`、`calibration_session_url`。

非回环场景下这些 `/preview` URL 仍受 Bearer 保护。前端必须通过统一鉴权 `fetch` 取得图片或下载文件，再使用 Blob/Object URL 展示或触发下载；不得把受保护 URL 直接交给不能附加 `Authorization` 的 `<img src>`、普通下载链接或原生 `EventSource`。回环场景可继续直接使用 URL。

### 1.5 默认值来源

连接参数由 `laser_tools::resolve_connection_spec` 解析：请求字段优先，缺失时回退 `.env` 的 `GRBL_DEFAULT_PORT`、`GRBL_BAUDRATE`（115200）、`LASER_NETWORK_HOST`、`LASER_NETWORK_TELNET_PORT`（23）、`LASER_DEFAULT_CONNECTION_MODE`（network）。请求中可用的连接键：`connection_mode` / `preferred_mode`、`port` / `serial_port`、`baudrate` / `baud`、`network_host` / `host`、`network_telnet_port` / `telnet_port`。文中写"设置默认"即指此。

### 1.6 浏览器来源（CORS）与来源校验

自 S1（技术书 §6.5 / D11）起，后端**不再返回** `Access-Control-Allow-Origin: *`。判定只看请求头 `Origin`，精确匹配 scheme + host + port：

| 允许的来源 | 说明 |
|---|---|
| `http://tauri.localhost`、`tauri://localhost` | Tauri 内嵌页（Windows / Android / macOS / Linux） |
| 与请求 `Host` 同源（`http://<Host>` 或 `https://<Host>`） | MOSS 自己提供的页面，含局域网浏览器访问 |
| `http://127.0.0.1:5173/5177/5178`、`http://localhost:5173/5177/5178` | 前端开发 / 预览端口；**仅回环请求**时允许 |
| `MOSS_ALLOWED_ORIGINS` | 额外来源，逗号分隔，默认空；含 `*`、缺少 scheme 或带路径时宿主启动失败 |

| 请求 | 处理 |
|---|---|
| 不带 `Origin`（插件、MCP 桥接、curl、进程内调用、局域网配对客户端） | 行为不变；响应不带任何 `Access-Control-Allow-*` 头 |
| 带允许的 `Origin` | 正常路由；响应回显 `Access-Control-Allow-Origin: <origin>`、`Vary: Origin`、`Access-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS`、`Access-Control-Allow-Headers: Content-Type, Authorization, Accept, X-Moss-Desktop-Session`；`OPTIONS` 另带 `Access-Control-Max-Age: 86400` |
| 带不允许的 `Origin`，方法为 `GET` / `HEAD` | 正常路由但不带任何 CORS 头（跨站页面读不到响应） |
| 带不允许的 `Origin`，其他方法（含 `OPTIONS` 与 `text/plain` 简单请求） | 403 `{"success":false,"result":"请求来源不在允许范围内","detail":{"error_code":"origin_not_allowed"}}`，**不进入路由、不执行业务** |

**回环连接的 Host 校验（防 DNS 重绑定）**：对端为回环地址且请求带 `Host` 时，`Host` 的主机名必须是 IPv4 / 方括号 IPv6 字面量、`localhost`（不区分大小写）或 `MOSS_ALLOWED_ORIGINS` 中出现的主机名，否则**任何方法**（含 `GET`）都返回 403 `{"success":false,"result":"请求的主机名不在允许范围内，请用 127.0.0.1 或 localhost 访问","detail":{"error_code":"host_not_allowed"}}`，不进入路由。原因：攻击者把自己的域名解析到 127.0.0.1 后，浏览器认为页面与 MOSS 同源，`Origin` 校验和 CORS 都拦不住，但请求的 `Host` 仍是攻击者域名。非回环连接不做此项（仍由 §5 的 Bearer 鉴权保护），不带 `Host` 的非浏览器请求不受影响。

执行顺序：Host 校验 → 来源校验 → §5 鉴权 → 路由。

## 2. GET 路由

| 路径 | 参数 | 成功 `result` | 失败 |
|---|---|---|---|
| `/api/health` | 无 | `{"status":"ok"}` | 无 |
| `/api/system/info` | 无 | `{actual_port, preferred_port, is_fallback, web_host, mcp:{configured, connected, endpoint(token 脱敏，未配置为空串), client_info, last_request, last_request_time, messages_count, tools_count, protocol_version, recent_events[], endpoints[], core_tool, safety_gate, estop_channel}, voice:{mode, device_sessions}}`。`configured` 只表示填了接入点地址；`connected` 表示桥当前与接入点保持连接；`client_info`、`last_request`、`recent_events` 无真实数据时为空，`messages_count` 为真实收到的帧数（无则 0），`tools_count` 为当前开关下实际暴露的工具数（安全 19 / 开发者 20，再减去严格模式不开放的与用户关闭的工具）；`endpoints[]`（NV8）为每个接入点的 `{id, endpoint(脱敏), connected, connections, last_error}`，`id` 为 `primary` 或 `extra-N`，`connected` 为任一接入点已连接。`voice.mode` 为 `official` / `self_hosted` / `builtin`（取自运行目录 `settings.json`）；`device_sessions` 在自建链路下来自接入点健康检查，在内置链路下来自内置语音服务（未运行为 `null`），官方链路为 `null` | 无；非回环也免鉴权 |
| `/api/ui/runtime-config` | 无 | `{connection:{default_mode, preferred_modes:[默认, 另一], serial:{default_port:"", baudrate:115200}, network:{default_host:"", transport:"telnet", http_port:80, telnet_port:23, timeout:5.0}}, upload:{gcode_suffixes:[".gcode",".nc"]}}` | 无 |
| `/api/ui/serial-ports` | 无 | `{ports:[…], default_port, baudrate}`；只列系统真实识别到的串口 | 无 |
| `/api/ui/discover-controllers`（**POST**，body `{}`；用 POST 防止跨站页面用无 Origin 的 GET 触发扫描） | 无 | `{controllers:[{hostname, ip, telnet_port, http_port}], hint}`；mDNS 浏览约 2 秒，不向设备发命令 | 无 |
| `/api/ui/material-options` | 无 | `{materials:[{name, thicknesses:[数字升序]}]}` 按 name 排序 | 500 `读取材料选项失败` |
| `/api/ui/material-recommendation` | `material` 必填、`laser_mode` 必填、`thickness_mm`、`engraving_mode` 可选、`power_scale` 可选 | 白名单投影：`material, thickness_mm, matched_thickness_mm, match, laser_mode, laser_max_power, feed_rate, passes`；engrave 且有值时加 `engraving_mode` | 400 `material 和 laser_mode 不能为空` / `读取材料推荐失败` |
| `/api/material-lab/export` | 无 | `{params_file, library:{version:1, power_scale, materials}}` | 500 |

### 2.1 `/preview` 文件服务

参数：`path` 必填；`download` 为 `1` 或 `true` 时附加 `Content-Disposition: attachment; filename="<文件名>"`。没有 `filename` 参数。

白名单，两条都要满足：

1. 后缀属于 `.png .jpg .jpeg .bmp .gif .webp .gcode .nc .json`。
2. 解析后位于运行目录的 `previews/`、`uploads/`、`generated_assets/` 之一（`ApiService::with_preview_roots`）。

成功：文件原始字节，`Content-Type` 按扩展名。失败一律 **404** JSON：`缺少 path` / `不支持预览该文件类型` / `文件不存在或不在允许的预览目录内` / `文件不存在`。

### 2.2 静态服务

`/`、`/draw`、`/draw/*`、`/material-lab` 返回嵌入的 `frontend/dist/index.html`，`/assets/*` 返回同目录静态资源（`rust-embed`，路径经 `safe_asset_path` 过滤）。`/material-lab` 是前端路由，首屏数据来自 §2.3。

### 2.3 `/material-lab` 初始数据来源（已决定 2026-09-14：新增 `GET /v1/material-lab/bootstrap`）

决定：不再在 "组合 17 路由" 与 "新增 /v1" 之间二选一，直接新增 `GET /v1/material-lab/bootstrap`，作为材料实验室页面唯一的首屏初始化接口。理由：组合 4~5 个接口拼首屏必然出现分批到达导致的闪烁与竞态，且接口不定则阶段 6（API）与阶段 7（前端）无法并行。

- 响应包装与 `/api/*` 相同：`{"success": true, "result": {...}}`；`result` 为与旧 `__MATERIAL_LAB_DATA__` 语义等价的单个 JSON 对象，一次请求返回页面首屏所需全部数据（材料列表与选项、最近记录、运行时配置、串口列表等，以阶段 1 提取的字段清单为准）。
- 字段表（Rust 实现）：

  | 字段 | 类型 | 来源 | 说明 |
  |---|---|---|---|
  | `version` | int | 恒为整数 `1`（归档规格 §22 `version` 决定） | 不再透传文件值 |
  | `materials` | object | 材料库 `materials` 嵌套结构，与 `material_params_tool` export 同构，按 db-schema.md §3.6.2 冻结 | 主体数据；含内置预设，用户库文件缺失时即为预设，不是 `{}` |
  | `error` | string \| null | 材料库加载错误 | 文件缺失不是错误（`null`）；只有文件存在但无法解析或 `version` 无法归一化时非空，此时 `materials` 为空对象 |
  | `prototype` | bool | 固定 `false` | 保留 |
  | `defaults.network_host` | string | 固定 `""` | 不猜设备地址 |
  | `defaults.network_telnet_port` | int | 固定 `23` | |
  | `source_path` | string | `LASER_MATERIAL_PARAMS_FILE` 的文件名部分，未设置为 `""` | 任何请求不返回目录成分 |
  | `backup_path` | string | 固定 `""` | |

- 材料库文件缺失或未配置时返回内置 4 种材料，`error` 为 `null`。
- 前端 `/material-lab` 挂载时只调用本接口一次；后续增删改仍走现有 `/api/material-lab/*` 路由，成功后重新拉取 bootstrap 或局部刷新。
- 认证规则同 §5：回环地址免认证，非回环要求 bearer。只读接口，不涉及设备。

## 3. POST 路由

### 3.1 `/api/upload`

| 字段 | 类型 | 必填 | 规则 |
|---|---|---|---|
| filename | string | 是 | 去路径与 `<>:"/\|?*` 及控制字符；后缀限 `.png .jpg .jpeg .bmp .gif .webp .gcode .nc`（**无 .json**） |
| content_base64 | string | 是 | 允许 `data:…;base64,` 前缀；严格 base64 |

成功 `result`：`{file_path, filename, size, suffix, preview_url}`，`file_path` 与 `filename` 都是保存后的文件名。保存名 `{unix 秒}_{sha1 前 10}_{stem 前 32}{suffix}` 到运行目录 `uploads/`；不登记数据库。
失败 400：`不支持上传该文件类型`（detail `{filename, allowed}`）、`上传内容为空`、`上传内容不是合法 base64`、`上传文件为空`。不做图片魔数校验。

### 3.2 `/api/draw/preview`

| 字段 | 类型 | 必填 | 规则 |
|---|---|---|---|
| image_data_url | string | 是 | `data:image/…;base64,` 图片；解码并校验为图片，尺寸为 0 时 400 |
| scene_json | object 或 JSON 字符串 | 否 | 存为 `<image>.excalidraw.json` |
| material | string | 是 | |
| thickness_mm | float | 是 | >0 |
| mode | string | 否 | `raster`（默认）或 `outline` |
| width_mm、height_mm、pixel_size_mm | 可选正数 | 否 | 缺省按图片像素与默认点距 |
| 连接字段 | 见 §1.5 | 否 | 只记录，不连接设备 |

后端直接生成光栅或轮廓 G-code（`generate_draw_artifacts`），写入运行目录 `generated_assets/`，创建 `source_type="draw"` 的 workflow 并冻结 `confirmation_snapshot`。
成功 `result` 含 `workflow_id`、`status=preview_ready`、`gcode_file`、`gcode_sha256`、`image_file`、`time_estimate`、`draw_lab:{job_summary, content_fingerprint, uploaded_image_file, scene_file}` 与 §1.4 的 `web`。
失败 400：各校验文案；`保存画板素材失败: …`。前端未收录的字段（`threshold`、`dither_algorithm`、`raster_quality_strategy` 等）当前被忽略。

### 3.3 `/api/connection/check`

| 字段 | 类型 | 默认 |
|---|---|---|
| connection_mode / preferred_mode | string | 空或 `auto/default/按配置` 走配置默认；别名 serial/usb/com/串口、network/net/wifi/http/telnet/网络 |
| auto_fallback | bool | true |
| port / serial_port | string | 设置默认 |
| baudrate | int | 设置默认 |
| network_host / host | string | 设置默认 |
| network_transport | string | `telnet` |
| network_http_port、network_telnet_port | int | 设置默认 |
| network_timeout | float | 设置默认 |
| include_detail | bool | false |

**总是 `success:true`**。`result`：`{preferred_mode, selected_mode|null, fallback_used, attempts:[{mode, target, connected, summary, laser_mode_ok, firmware_settings(仅 include_detail), timeout_seconds}], serial_connected, network_connected, summary, machine_limits:{x_travel_mm, y_travel_mm, z_travel_mm}|null, features:{speed_map, has_air_assist, has_door}|null}`。`machine_limits` 来自 FluidNC `$CD` 的 `max_travel_mm` 或经典 GRBL 的 `$130/$131`；前端用它约束画板范围。只有参数类型错或模式非法才 400。检查会真实握手（`$$` 到 `ok`、要求 `$32=1`），不发送运动或出光指令。

### 3.4 `/api/text-task/generate`

| 字段 | 类型 | 必填 | 默认 |
|---|---|---|---|
| text、material | string | 是 | `text 不能为空` |
| thickness_mm | float | 是 | >0，否则 400 `thickness_mm 必须是大于 0 的数字` |
| engraving_mode | string | 否 | `raster`；`outline` 走字体矢量轮廓 |
| font_size_mm | float | 否 | 10.0 |
| letter_spacing_mm | float | 否 | 0.5 |
| font_path | string | 否 | 缺省使用内置字体 |
| 连接字段 | 见 §1.5 | 否 | 只记录 |

后端用材料库推荐参数生成 G-code 与预览 PNG（运行目录 `generated_assets/`），创建 `source_type="text"` 的 workflow 并冻结快照。
成功 `result` 含 `workflow_id`、`status=preview_ready`、`gcode_file`、`gcode_sha256`、`image_file`、`font_path`、`material`、`thickness_mm`、`width_mm`、`height_mm`、`glyphs`、`path_optimization`、`time_estimate:{estimated_seconds, estimated_minutes, laser_seconds, non_laser_seconds}` 与 §1.4 的 `web`。
材料安全门：后端调用 `laser_core::safety::evaluate_material`；PVC、聚氯乙烯及明确危险材料返回 400 `material_safety_blocked`，仅写“塑料”等成分不明描述时同样返回 400 并要求澄清。MCP 工具 `generate_text_laser_task_tool` / `generate_text_image_gcode_tool` / `ai_laser_gcode_tool` 与 `laser_workflow_tool preview`（带 `text`）都转发到本路由；工具的 `font_size` 按 `32px≈1mm` 折算为 `font_size_mm` 并限制在 3～80。

### 3.5 `/api/text-task/send`

白名单字段，其余全部丢弃：

| 字段 | 说明 |
|---|---|
| workflow_id | 必填 |
| confirmed | bool，默认 false；HTTP 层校验，缺失或为 false 时 400 `请确认发送安全选项 (confirmed=true)` |
| connection_mode | 规则同 §3.3 |
| run_in_background、wait_for_response | 默认 true |
| serial 时 | `port \| serial_port`、`baudrate` |
| network 时 | `network_host`（默认 ""）、`network_http_port`、`network_telnet_port`、`network_timeout`；`network_transport` 强制 `telnet` |

确认请求只需提交 `workflow_id` 与 `confirmed=true`（以及本表允许的连接参数）；客户端不提交快照 SHA。服务端加载该 workflow 的 `confirmation_snapshot`，重新计算当前 G-code 文件 SHA-256，并按 §3.6 的门禁顺序校验；任一不一致均拒绝发送。本路由与 `/api/workflow confirm_send` 等价。

### 3.6 `/api/workflow`

`action` 默认 `status`；`workflow_id`。支持的 action：

| action | 说明 | 失败 |
|---|---|---|
| `preview` / `plan` / `create` | `source_type=text` 时等价于 §3.4；`source_type=image` 需要 `image_file`（已上传文件名，位于预览白名单目录）、`material`、`thickness_mm`（默认 3.0）；`source_type=gcode` 需要 `gcode_file`。生成产物、创建记录并冻结快照 | 400 `未指定图片素材路径` / `找不到指定的素材图片文件` / `未指定 G-code 刀路路径` / `找不到指定的 G-code 刀路文件` / `图片尺寸为 0` |
| `status` | 返回记录并叠加内存中的实时 `send_result`（`running, total_lines, lines_sent, target, phase, error, sd_hash_verified, warning`） | 400 `workflow_id 不能为空`；404 `未找到 workflow` |
| `recent` | 最近记录列表 | |
| `confirm_send` / `send` | 必须 `confirmed=true`；门禁顺序：记录存在 → 状态可发送（`sending/completed/failed/cancelled` 拒绝，`workflow_not_sendable`）→ 快照存在（`confirmation_snapshot_missing`）→ 物理文件 SHA-256 重算一致（`send_gate_rejected`）→ 设备连接、握手、`$CD`、预检、行校验（§1 增补）→ 同设备无在途作业（409 `device_busy`）。通过后后台逐行发送，响应 `status=sending` 与 `send_result`。可带 `offline_sd=true`（或 `send_mode=sd_offline`）走 SD 脱机模式 | 400 / 404 / 409 |
| `resume` | 从 `start_from_line` 断点续传；同样要求 `confirmed=true`、快照与 SHA 一致，且记录未全量完成（`workflow_completed` / `workflow_not_resumable`） | 400 / 404 |
| `cancel` | 置取消并走取消序列；不承诺物理停机 | 400 / 404 |

严格模式（`MOSS_FUSION_STRICT=true`，§5 桌面会话）下，`confirm_send` / `send` / `resume` 在上述门禁之后、连接设备之前追加授权检查：桌面会话照常执行；小智桥接 / 局域网 / 网关返回 202 `result.status="approval_required"`，记录状态不变；本机回环无会话返回 403 `strict_mode_blocked`。标定矩阵发送（§3.8）与文字任务发送（§3.5）相同。

未知 action → 400 `未知 workflow action`。`update`、`feedback`、`regenerate` 不是 workflow action：反馈调参与重生成是独立 MCP 工具。响应经 §1.4。

### 3.7 `/api/material-lab/preview`

| 字段 | 类型 | 必填 | 默认 / 规则 |
|---|---|---|---|
| material | string | 是 | |
| thickness_mm | float | 是 | >0 |
| laser_mode | string | 否 | `engrave`；`engrave/cut` |
| engraving_mode | string | 否 | `raster`；cut 时置空 |
| matrix_kind | string | 否 | `auto` 随 laser_mode；`raster/outline/engraving` → engrave；否则须 `engrave/cut` |
| rows、columns | int | 否 | 5、5 |
| power_min、power_max、speed_min、speed_max | int | 否 | -1 |
| passes、pass_min、pass_max | int | 否 | 0、1、0 |
| cell_size_mm、gap_mm、x0、y0、fill_step_mm | float | 否 | 5.0、2.0、0、0、0.8 |
| network_host | string | 否 | "" |
| network_transport | string | 否 | `telnet` |
| network_http_port、network_telnet_port、network_timeout | int、int、float | 否 | 设置默认 |
| wait_for_response、run_in_background | bool | 否 | true |

Rust 实现：只生成不出光。区间字段缺省或为 -1 时以材料推荐参数为中心取 ±40%；`rows/columns` 限 1..25。成功 `result` 为完整 calibration 记录：`calibration_id`（`cal-<ts>-<seq>`）、`status=prepared`、`cells[]`（`cell_number/id/row/column/x_mm/y_mm/size_mm/power/speed/passes`）、`cell_count`、`gcode_file`、`gcode_sha256`、`confirmation_snapshot`、`time_estimate`、`web.gcode_download_url`。记录写入 `calibrations` 表（无 SQLite 时写内存表）。

### 3.8 `/api/material-lab/send`

| 字段 | 规则 |
|---|---|
| calibration_id | 必填 |
| confirmed | **必须为 true**，否则 400 `发送测试矩阵需要 confirmed=true` |
| network_host | 空且无默认 host → 400 `请填写设备 host/IP` |
| network_transport | `telnet` |
| network_http_port、network_telnet_port、network_timeout | 设置默认 |
| wait_for_response、run_in_background | true |

Rust 实现：与 `/api/workflow confirm_send` 同一套门禁——记录不存在 404 `calibration_missing`；`sending/completed/failed/cancelled` 状态 400 `calibration_not_sendable`；物理刀路 SHA-256 重算不一致 400 `send_gate_rejected`；设备不可达/握手失败 400 `device_unreachable`/`device_not_ready`；同一设备已有在途作业 409 `device_busy`。通过后由 JobWorker 后台逐行下发，响应 `status=sending` 与 `send_result{running,total_lines,lines_sent,target}`，终态由发送线程写回记录。`POST /api/material-lab/status {calibration_id}` 返回带实时进度的记录，供前端轮询；缺 id 400 `calibration_id 不能为空`，不存在 404 `未找到标定矩阵`。

### 3.9 `/api/material-lab/select-cell`

`calibration_id` 必填；`cell_number` int 默认 0；`thickness_mm` float 默认 0.0；`material`、`laser_mode`、`engraving_mode`、`notes` string 默认 ""。

Rust 实现：`cell_number` 必须在 1..cell_count；缺省的 material/thickness/laser_mode/engraving_mode 从 calibration 记录回填；把单元格的 `power→laser_max_power`、`speed→feed_rate`、`passes`（及 `pixel_size_mm`）以 `source=calibration` 经 `manage(save)` 写入材料库文件与 SQLite，并在记录上写 `selected_cell`。成功 `result`：`{calibration_id, cell_number, selected_cell, materials, speech}`。

### 3.10 `/api/material-lab/manage`

`action` 必填，`save / delete / import / export`，否则 400。

| action | 字段 | 结果 |
|---|---|---|
| export | 无 | 工具透传；`version` 恒为整数 1，`materials` 含内置预设（db-schema.md §3.6.1、§3.6.2） |
| import | `library_json \| params_json`、`import_mode` 默认 `merge` | 工具 result + `materials` 全量快照 + `backup_path`。`version` 校验严格：只接受 JSON 数值 `1`/`1.0`，`"1"` 或其他值一律 400 `导入材料库格式不支持：需要 version=1 的 JSON 对象`。`backup_path` 只返回文件名 |
| delete | `material` 必填、`thickness_mm` float、`laser_mode` 默认 engrave、`engraving_mode` 默认 raster、`confirmed` **必须 true** 否则 400 `删除当前参数需要 confirmed=true` | 工具 result + `materials` |
| save | delete 的字段 + `laser_min_power` 0、`laser_max_power` 800、`feed_rate` 1200、`travel_rate` 设置默认、`pixel_size_mm` 0.1、`threshold` -1、`passes` 1（数值字段在入库边界统一转为数字，无法转换返回 400）、`aliases`（数组或中英文逗号分隔）、`notes`、`source` 默认 manual、`machine_profile_id` 默认 `yisu-v1-100x100`、`confidence`、`evidence`（对象转 JSON）、`version_mode` 默认 update。**原子批量扩展（D9 已拍板）**：除单厚度参数外，允许传入 `thicknesses` 对象/列表进行多厚度一次性原子保存，后端在单个数据库事务内完成全部更新，失败整体回滚；禁止前端通过循环发送 N 次 HTTP 请求进行多厚度保存 | 工具 result + `materials` |

### 3.11 `/api/material-lab/recent`

GET 与 POST 都接受；忽略请求体（仍受 64 KiB 限制）。从 workflow 按 `updated_at` 倒序取第一个含完整参数键的记录。
成功 `result`：`{workflow_id, material, thickness_mm, laser_mode, engraving_mode, params}`。
失败 400：`最近的激光 workflow 中没有可保存的完整材料参数`。

### 3.12 `/api/device/framing`

真实驱动机器沿外接矩形走边框，用于校对材料摆放位置。字段：`min_x`、`min_y`（默认 0）、`max_x`、`max_y`（默认 100）、`feed_rate`（默认 2000）、`power`（默认 1，极低功率光斑）与 §1.5 连接字段。流程：生成 framing G-code → 打开传输 → 握手 → `$CD` → `JobWorker::send_lines`。成功 `result`：`{message:"走边框完成", bounds}`；失败 400（生成、连接、握手、行校验）或 500（执行失败）。不经 workflow 快照，也不需要 `confirmed`，改动时按发送链路对待。严格模式下按 R3 处理（授权规则同 §3.6）。

### 3.13 `/api/safe-action`

请求体为 `laser_contracts::LaserSafeActionArgs`：`action` 必填，可选 `confirmed`、`dry_run`、`x`、`y`、`feed_rate`、`power` 与连接字段。动作集合见 [开发者安全入口](safety.md) §2；`status` 与进给/功率倍率不需确认，其余需 `confirmed=true`。成功 200，失败 400，响应体 `{success, result, detail}`。MCP `laser_safe_action_tool` 在有宿主时转发到本路由。严格模式下，除 `status`、`emergency_stop`、`soft_reset`、`laser_off`、倍率复位与降低外的动作按 R3 处理（授权规则同 §3.6，判定函数 `laser_contracts::safe_action_requires_approval`）。

2026-10-10：HTTP 停止/查询请求可另带 `job_id`，仅限 `emergency_stop`、`soft_reset`、`status`，按登记任务使用其连接；编号未知或与显式目标不一致时失败，不改用其他设备。无编号时优先匹配显式连接目标；多个活动任务且无目标时拒绝随意选择。无活动任务且无编号时仍可通过明确的连接配置发送停止，不能依赖任务列表存在。`dry_run=true` 不写入活动任务的实时字节。成功只证明请求发送，不保证设备物理停止。

### 3.14 `/api/jev/status`、`/api/jev/reveal`、`/api/jev/test`、`/api/jev/feedback`

Jev 是可选的加工反馈判断服务，只接入 OpenRouter。固定模型 `typesafe/jev-1.13`，固定 `POST https://openrouter.ai/api/alpha/decisions`，Bearer 认证；请求含 `model`、`state`、`questions.feedback_type.{type:"choice",instructions,criteria}`，响应读取 `answers.feedback_type.{type,choice,confidence}`。不使用 Chat Completions 或 TypeSafe 直连接口，不跟随重定向，超时 5 秒，响应上限 64 KiB。协议依据 OpenRouter 官方 `openapi.json`（2026-09-22 核对）。

`GET /api/jev/status` 返回 `enabled`、固定 `endpoint`、`model`、`provider`、`configured` 和 `masked_key`，不返回完整 Key。`POST /api/jev/reveal` 沿用现有鉴权规则，返回完整 Key；无 Key 为 404。`POST /api/jev/test` 使用已保存的 OpenRouter Key 测试 Decisions 契约，不连接设备。旧 TypeSafe Key 不能用于 OpenRouter，升级后需在设置中替换为 OpenRouter Key。

`POST /api/feedback`（兼容别名 `/api/jev/feedback`）接收 `{workflow_id|task_id, feedback_text}`。必须关联已有 workflow，反馈长度为 1..2000 字符。原参数读取 `artifacts.actual_params`；无此快照的旧任务或上传 G-code 必须显式提供 `laser_max_power`、`feed_rate`、`passes`、`power_scale`，只提供建议，不能自动采用。参数须为有限数字：`0<power<=power_scale`、标尺 1..100000、速度 100..6000 mm/min、次数为 1..5 整数。新生成预览使用 1000 标尺，反馈参数中的功率、速度须为整数；超限或缺失返回 400，显式参数与实际快照冲突返回 409，不猜默认参数。

分类：`too_dark/too_light/not_cut_through/rough_edge/broken_line/acceptable/unknown`。模型只分类；本地确定性规则转换为有界参数。置信度低于 0.8 时按 `unknown` 处理（保守策略值，未经实机校准）。`acceptable/unknown` 不调整。服务未启用、缺 Key、HTTP/协议错误时由 API 统一回退本地规则；否定、多个问题及不明确反馈不调整。MCP 与 Web/桌面共用该结果，不再透传模型原始 JSON。

成功 `result` 包含 `feedback_id/workflow_id/feedback_text/created_at/source/fallback_reason/feedback_type/decision/original_params/suggested_params/adopted_params/adopted_workflow_id/can_adopt/speech`。`source` 为 `openrouter` 或 `local_rules`。首次仅保存建议，`adopted_params=null`；不保存 Key、原始网络响应、文件路径到模型输入。历史放在 workflow payload 的 `feedback_history`，通过 RuntimeStore 原子合并，保留当前作业状态；`GET /api/feedback/history?workflow_id=...` 读取历史。`jobs` 表仍无新增写入方，不伪造独立作业。

`POST /api/feedback/accept` 接收 `{workflow_id,feedback_id,confirmed:true}`，生成独立 `preview_ready` workflow，返回更新后的反馈记录及 `adopted_workflow_id`。成功保存新预览后才记采用参数，重复请求返回已采用记录。不会改原文件、直接修改材料库或发送硬件。新文件实际使用建议的功率、速度和次数，并重新冻结 SHA；发送仍须对新 workflow 独立 `confirm_send` 和全部既有门禁。过期或加工中的任务返回 409。MCP 采用通过现有 `regenerate_text_laser_task_tool(task_id,feedback_id,confirmed=true)`，不新增工具。

新 workflow 与原反馈的采用状态在同一 SQLite 事务（内存模式同一把锁）提交；失败不会留下可查询的孤立任务。普通 workflow 状态写回保留数据库中现有反馈历史，避免旧快照覆盖历史。新任务另有 `feedback_origin`，界面切换后仍可显示本次来源反馈。`feedback_params` 是内部生成参数，公开 text/draw/workflow 请求不能直接传入，必须走采用接口。画板当前实际生成策略仍是光栅，摘要按实际文件显示，不把输入的 outline/cut 标签当成已实现的生成策略。

### 3.15 `/api/settings`

`{web_port, mcp_endpoint, jev_enabled, jev_api_key, jev_delete_api_key}`。`mcp_endpoint` 保留以兼容旧前端、独立 `moss-host` 与手机端；桌面端页面改在语音中心修改（技术书 §9.5.1），由语音中心直接写 `settings.json` 并立即重连。普通 Jev 配置写入运行目录 `settings.json`；Key 仍以原有方式存储于 SQLite `app_secrets`，本次不改变存储安全方案。旧 `jev_endpoint` 字段仅接受空值或固定 OpenRouter Decisions 地址，其他地址返回 400；已保存的旧地址不参与请求路由。`jev_delete_api_key:true` 删除已保存的 Key。

## 4. 需要在实现时保持的怪异行为

这些是现状，照搬，不修：

1. `/api/material-lab/export` 与 `manage(action=export)` 两个入口都保留，但 `/api/material-lab/export` 的响应形态是 `{params_file, library}`，`manage(export)` 返回工具结果。
2. `confirmed` 在 HTTP 层统一校验：`text-task/send`、`workflow confirm_send/resume`、`material-lab/send`、`manage(delete)` 缺失都返回 400；工具层门禁是第二道防线。
3. `manage(save)` 的功率、速度、阈值、次数、像素尺寸等数值字段在入库边界统一转为数字；字符串 `"800"` 入库为数值 `800`，无法转换时报 400。导出统一输出数字。
4. `/api/upload` 不校验图片魔数，`draw/preview` 校验。
5. `/preview` 的所有失败都是 404，包括参数缺失。
6. `/api/material-lab/recent` 同时接受 GET 与 POST。

## 5. `/v1` 接口与鉴权

| 路径 | 方法 | 用途 | 认证 |
|---|---|---|---|
| `/v1/capabilities` | GET | `{http:true, serial:true, network:true, shell:false, tray:<桌面平台>, command_execution:<MOSS_ENABLE_DEVELOPER_COMMANDS=true>}` | 公开 |
| `/v1/events` | GET SSE | 一次性事件：返回单条 `event: ready`，`data` 为 `{workflow_id, status}`（按 `?workflow_id=` 查库，缺省 `connected`），随后关闭连接。不是持续推送；前端不使用 | 同旧路由规则 |
| `/v1/material-lab/bootstrap` | GET | 材料实验室首屏一次性初始化数据，字段按 §2.3；只读，不触设备 | 同旧路由规则 |
| `/v1/auth/challenge` | POST | 生成 6 位数字配对码，`result:{pairing_code, expires_at}`，5 分钟过期；**仅回环地址允许**，否则 403 | 回环 |
| `/v1/auth/pair` | POST | `{pairing_code}` → `result:{token, pairing_id}` | 非回环时唯一免 Bearer 的数据接口 |
| `/v1/approvals?status=pending` | GET | 审批列表（新的在前，最多 50 条，先处理到期）：`result:{approvals:[…], server_time, ttl_seconds}`；每条为 `ApprovalRecord` 加 `target:{mode, label}`（串口显示端口名，网络设备显示“网络设备”，**不含地址**）；`status` 可为 `pending/approved/rejected/expired/executed/failed/all`，其他值 400 | 桌面会话 |
| `/v1/approvals/{id}/approve` | POST | `{digest}`：按当前记录重算摘要，与审批卡提交值和创建时的值都一致才批准，随后立即以审批身份重放原请求（使用审批保存的连接参数，走全部现有门禁）。成功 200 `result:{approval, result}`；失败透传执行错误码，`detail.approval` 为最新审批状态 | 桌面会话 |
| `/v1/approvals/{id}/reject` | POST | `{reason?}` → `result` 为拒绝后的审批 | 桌面会话 |

认证规则（在 §1.6 来源校验之后）：来源为回环地址时所有路由免认证。非回环时，`GET /api/health`、`/v1/capabilities`、`/api/system/info`、页面与静态资源（`/`、`/draw`、`/material-lab`、`/assets/*`、`/vendor/*`）公开，`POST /v1/auth/pair` 免 Bearer，其余数据接口（`/api/*`、`/v1/*`、`/preview`）要求 `Authorization: Bearer <token>`，缺失返回 401 `需要 Bearer 鉴权`，无效返回 401 `Bearer 鉴权失败`。

审批请求编号（2026-10-10）：可信语音 device/session、局域网 pairing_id 或网关 session 构成调用方作用域；新记录 `request_id` 为 `scope-v1:` 哈希。相同作用域和编号仅在执行参数及摘要一致时复用，不一致返回 409 `approval_request_conflict`；超过 128 字符拒绝。旧记录保持可读；旧 McpBridge 没有独立可信身份，不按自报编号复用审批。`voice_session_revoked` 表示旧语音会话不再有调用权，重新允许设备不恢复旧会话和旧审批。

桌面会话（技术书 §5.3，2026-10-06）：Tauri 桌面壳启动时调用 `ApiService::enable_desktop_session` 生成 32 字节随机会话值，页面经命令 `get_desktop_session` 取得、只存内存（该命令只交给 `main` 窗口且页面来源等于本进程后端地址，其他窗口或来源返回错误，2026-10-13），所有 API 请求带 `X-Moss-Desktop-Session`。该头**只在回环连接上有效**；值不对、或后端没有开启会话（独立 `moss-host serve`）、或来自非回环连接时一律 403 `desktop_session_invalid`（前端重新取会话后只重试一次）。有效时调用方上下文为 `Desktop`：严格模式下可直接发送，并且是唯一能调用 `/v1/approvals*` 的身份；其他身份调用审批接口得到 403 `desktop_session_required`。

审批相关错误码：`approval_not_found` 404、`approval_expired` 410、`approval_consumed` / `approval_rejected` / `approval_not_approved` / `approval_mismatch` 409、`approval_invalid_request` / `target_required` 400、`approval_unavailable` 503；严格模式下非桌面来源的 R3 请求得到 202 `result.status = "approval_required"`（`approval_id`、`expires_at`、`summary`、`speech`），回环无凭据得到 403 `strict_mode_blocked`。

语音中心（技术书 §9.4.6，P4-07）**不是 HTTP 接口**：以 Tauri 命令提供，只能由 MOSS 桌面窗口调用；改写设置、启停进程、测试/保存模型、读日志、本地模型下载删除、设备允许与改名，以及视觉/音乐/Agent 全部命令，另要求调用窗口为 `main` 且来源等于本进程后端地址，否则返回 `voice_acl_denied`（2026-10-13）；只读的 `voice_status`、`voice_mode_changes`、`voice_builtin_status` 不受此限；HTTP 后端（桌面内置、独立 `moss-host serve`、Android）都没有 `/v1/voice/*`，一律 404（A-68）。命令与参数：`voice_status()`、`voice_save_settings({settings})`、`voice_mode_changes({mode})`、`voice_set_mode({mode})`（`official` / `self_hosted` / `builtin`）、`voice_server_start()` / `voice_server_stop()` / `voice_server_restart()`、`voice_server_log({tail})`、`voice_models_test({section, input})`（`llm` / `asr` / `tts`；`input` 含密钥，只在本次调用内存中使用）、`voice_models_save({input})`；`exit_application({stopServer})`。接入点与工具（NV8）：`voice_save_settings` 另接受 `add_extra_mcp_endpoint`（附加官方接入点，`ws://` / `wss://`、不含占位符、不能重复、最多 4 个）、`remove_extra_mcp_endpoint`（编号 `extra-N`）、`disabled_tools`（关闭的工具名数组，整体替换；未知工具或 `laser_safe_action_tool` 返回 400 `voice_settings_invalid`），写入 `settings.json` 顶层 `mcp_extra_endpoints` / `mcp_disabled_tools`（列表为空时删除键）并按需重连小智桥；`voice_status` 另含 `mcp`：`{endpoints[]: {id, endpoint(脱敏), active, connected, connected_at, connections, last_error, client, identity{fields, issued_at, expires_at, expired} | null}, extras_paused, max_extra_endpoints, tools[]: {name, description, enabled, can_disable, voice_hidden, calls, failures, denied, last_at, last_caller}}`。被关闭的工具经接入点或内置语音调用时返回工具错误 `tool_disabled`。内置语音服务（NV1）：`voice_builtin_status()`、`voice_builtin_start()`、`voice_builtin_stop()`、`voice_device_set_status({deviceId, status})`（`allowed` / `revoked`）、`voice_device_rename({deviceId, name})`、`voice_builtin_models_test({section, input})`（密钥留空时用已保存的密钥；识别、合成的 `input.kind` 可为 `doubao`——豆包流式，地址为 `wss://`、`model` 为资源 ID，二者留空用默认值，缺省 `openai`；外部服务端的 `voice_models_*` 不接受 `doubao`）、`voice_builtin_models_save({input, clear})`（`clear` 为要删除的分区；运行中会重启内置服务，返回 `restarted`）；本地模型（NV6）：`voice_local_save({settings})`（`asr` / `tts` / `vad` 开关、`vad_model` / `asr_model` / `tts_model`（选用的模型 id，须存在且类别相符，否则 400 `voice_settings_invalid`）、`tts_speaker`、`tts_speed`、`mirror`；运行中会重启内置服务）、`voice_local_model_download({id})` / `voice_local_model_cancel({id})` / `voice_local_model_delete({id})`（id：判停 `silero_vad` / `ten_vad`，识别 `sense_voice` / `sense_voice_2025` / `x_asr_streaming`，合成 `kokoro` / `kokoro_v11` / `matcha_zh_en`；运行中删除返回 409 `local_model_in_use`）；`voice_builtin_status` 另含 `local`（设置、`models[]` 的 `id`、`kind`（`vad` / `asr` / `tts`）、`engine`、`label`、`note`（优缺点）、`size`（含附加文件）、`streaming`、`voices{default, max, hint}`（仅多音色合成模型）、`installed` 与 `progress{state: downloading/verifying/installing/failed/cancelled}`、`helper_found`、`helper_running`、`ready`）。`voice_builtin_status` 返回 `mode`（`chat` / `echo`）、`link_mode`、`models`（每个分区的 `configured`、`key_set`、`kind`、地址与模型，**不含密钥**）、`last_error`；错误码另有 `builtin_start_failed`（409，端口被占用等）、`voice_device_not_found`（404）、`voice_device_invalid`（400）。内置服务对设备开放的端口只提供小智协议：`GET/POST /xiaozhi/ota/`（OTA 端口，默认 8003）与任意路径的 WebSocket（设备端口，默认 8000），见内置语音技术书 §13。失败统一为 `{code, message, status}`，常见 `code`：`server_already_running` / `server_external` / `server_not_running` / `server_not_owned`（409）、`voice_config_invalid` / `server_config_missing` / `voice_settings_invalid` / `model_input_invalid`（400）、`self_hosted_endpoint_missing`、`model_test_required`（409，保存内容与测试通过的不一致）、`server_full_module`（409，全模块部署只测不写）、`voice_mode_switch_failed`（已回滚）、`voice_desktop_only`（移动端）。返回内容中带令牌的地址均已脱敏，不含任何模型密钥。

配对语义（`AuthManager`）：配对码按来源 IP 限流，连续错误 5 次锁定 15 分钟，锁定时返回 429 `配对请求过于频繁，请稍后重试` 并带 `Retry-After`；配对码无效、已使用或已过期统一返回 400 `配对码无效或已过期`，不通过文案区分原因。token 为 32 字节 CSPRNG 随机值的 Base64，服务端只保存其 SHA-256 哈希与 `pairing_id`。**全部状态在进程内存中**，`auth_pairings` 表当前不写入，宿主重启后所有配对失效；没有撤销接口。非回环部署默认仅限受信任局域网明文 HTTP，禁止公网直连；跨网必须由前置代理终结 TLS。移动端复用同一套路由，不新增独立的危险操作接口。

### 5.1 第三方 MCP（NV9，2026-10-10）

桌面新增 `voice_mcp_status`、`voice_mcp_save({input})`、`voice_mcp_delete({name,revision,confirmed})`、`voice_mcp_reconnect({name,confirmed})`。只授予主窗口且在命令入口再次核对实际回环来源与桌面会话，没有 HTTP 配置／执行接口。输入、返回、凭据引用与失败语义见 [第三方 MCP 契约](../integration/moss-external-mcp.md)。

现有审批列表增加 `kind=external_tool`、`target.mode=external` 和 `external:{tool,arguments}`（仅有界脱敏预览）。批准仍只接受桌面身份，使用完整隐藏 request 和当前第三方连接／权限重算摘要；授权只消费一次，不能使用参数中的 `confirmed` 或 `approval_id` 绕过。外部审批与激光审批使用各自的执行分支，不会把外部参数变成设备命令。第三方结果不确定或结果存储失败不自动重试；连接恢复也不补发旧调用。

## 6. 黄金样本生成要求（阶段 1）

黄金样本采集自 Python 版（`tests/golden/api/`），Rust 版不再自动差分，保留作为字段形态与错误文案的参考。掩码字段：`workflow_id`、`task_id`、`calibration_id`、`job_id`、`created_at`、`updated_at`、所有绝对路径、`content_fingerprint`、`source_path`、`backup_path`、`params_file`（Python 返回绝对路径，Rust 只返回文件名，值本身不可比）。注意样本文件本身仍含采集机器的绝对路径。材料库样本比对时，Python 侧 `null`/`{}` 墓碑节点视为该格不存在（db-schema.md §3.6.2）。比对时 `result.web.*_url` 只比路径部分。

比对方法（归档规格 §20.1 级别 1）：响应体解析为 JSON 后 deep-equal，不比文本。HTTP 状态码、`success` 值、`result`/`detail` 结构必须相等；错误文案字符串逐字相等，因为用户会看到；数值按数值比较；键序与空白忽略。`/preview` 返回的文件按 SHA-256 比较。黄金样本保存原始响应，掩码与规范化在比对时进行。
