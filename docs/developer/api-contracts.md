# Web API 接口契约（`/api/*` 与 `/v1/*` 路由、文件与静态服务）

状态：2026-09-22 按 `crates/laser-api/src/lib.rs` 现状核对。路由表与字段以代码为准；黄金样本 `tests/golden/api/` 记录的是 Python 版响应，比对规则见 §6。
历史依据：原稿从 Python 版 `web_server.py` 提取（2026-09-14）。
用途：前端、MCP 工具转发（`laser_host::InProcessBackend` 直接调用同一 `ApiService::handle_http`）与测试共同遵守的 HTTP 契约。MCP 工具描述和小智提示词的自然语言调整不改变本文约定的字段、状态码和响应结构。

路由总表：

| 方法 | 路径 | 说明 | 详见 |
|---|---|---|---|
| GET | `/api/health` | 存活检查 | §2 |
| GET | `/api/system/info` | 端口、MCP 桥状态与遥测；非回环也公开 | §2 |
| GET | `/api/jobs/stop-targets` | 顶栏急停与激光任务进度浮层的后端原始任务目标：`result.jobs` 数组。除 `job_id`、`target`、`state` (`running` / `unknown`) 外，2026-11-02 起运行中的作业还带只读进度字段 `workflow_id`、`phase` (`streaming` / `awaiting_completion`)、`lines_sent`、`total_lines`、`elapsed_ms`、`mode`（取自 `ActiveJob::live_send_result`）。**纯只读**：不开设备连接、不写实时通道；状态未知不代表已停机，多个目标不得随机选择 | §2 |
| POST | `/api/jobs/acknowledge-stop` | 交互端操作者现场核实机器已停稳后清除指定非空 `job_id` 且 `confirmed:true` 的 `uncertain_jobs` 待确认停止记录（拒绝语音/MCP 桥/审批重放调用方，不支持无编号全量清空，不影响运行中的 `active_jobs`） | §2 |
| GET | `/api/ui/runtime-config` | 连接默认值与上传后缀 | §2 |
| GET | `/api/ui/serial-ports` | 系统串口列表 | §2 |
| POST | `/api/ui/discover-controllers` | 查找并只读识别局域网里的 GRBL / FluidNC（mDNS + 本网段 23 端口，逐台 `?` + `$I`；2026-11-09） | §2 |
| GET | `/api/ui/material-options` | 材料名与厚度 | §2 |
| GET | `/api/ui/material-recommendation` | 材料推荐参数 | §2 |
| GET | `/api/material-lab/export` | 材料库导出 | §2 |
| GET / POST | `/api/material-lab/recent` | 最近一条含完整参数的 workflow | §3.11 |
| GET | `/preview` | 受控文件服务 | §2.1 |
| GET | `/`、`/draw`、`/material-lab`、`/twin`、`/voice`、`/camera`、`/assets/*` | 嵌入的前端 | §2.2 |
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

### NV19 长期记忆（Tauri 桌面命令，非 HTTP 路由）

只由本机打包的 `voice-private` 窗口调用：权限单独放在 `capabilities/voice-context.json`（`windows:["voice-private"]`，无 `remote`），处理函数开头再执行 `require_voice_private_asset`。主窗口、局域网、手机端都没有这两个命令；移动端返回 `voice_desktop_only`。

- `voice_context_pick_folder()` → `null`（取消）或 `{directories, files}`。点击后由原生对话框选择文件夹，并在本机为**当前项目**建立资料索引；路径不经前端，也不出现在提示词里。没有当前项目时报 `context_no_project`。
- `voice_context_manage({input})`，`input.action` 取值：
  - `status` → `{retrieval, extraction, project:{id,name}|null, devices:[{id,name,model,firmware}], scopes:[{kind,id,total,active,candidates,chars}], legacy_summaries:[device_id]}`（只含已允许的设备）。
  - `set_options {retrieval?, extraction?, confirmed?}`：开启自动提取必须带 `confirmed:true`，否则 `context_confirm_required`；关闭不需要。`set_project {name}`（生成新项目编号并递增代次）、`clear_project`。
  - `list {scope:{kind,id}, category?, query?}` → `{rows}`（有 `query` 时为快速检索）；`deep_search {query}`（需要已配置的内置大模型，否则 `context_llm_missing`）；`create {scope, category, content, confirmed?, device_id?, expires_at?}`；`edit {id, revision, content, device_id?, expires_at?}`；`verify {id, revision, to:"confirmed"|"invalidated"}`（故障经验确认时写入“用户确认已解决”）；`delete {id, revision}`；`clear {scope, confirmed:true}`；`import_legacy {device_id}`；`export {scope}` → `{memories}`。
  - `extractions {scope}` → `{runs:[{id, started_at, finished_at, reverted_at, added, skipped:[{op,reason}]}]}`；`revert {run_id}` → `{removed, kept}`。
  - `knowledge_list` → `{roots:[{id,label,revision,indexed_at,directories,files}]}`；`knowledge_reindex {id}`；`knowledge_remove {id, revision}`。
  - `experience_record {device_id, symptom, checks[], conclusion, confirmed?}` → `{row, similar:[id]}`（设备必须登记了机型和固件）；`experience_recall {device_id, symptom}` → `{text}`。
  - `resume_get` → `null` 或 `{id, revision, updated_at, note:{decisions,todos,evidence,unverified}, text}`；`resume_save {note, expected?:{id,revision}}`（已有摘要时必须带 `expected`）。
- 每条记录 `row`：`{id, scope, category, content, verification, source_kind, source_ref, device_model, firmware, revision, created_at, updated_at, expires_at, state}`，`state` 为 `active / expired / invalidated / deleted`。
- 错误为 `{code, message}`：NV19-01 的 `context_*`，以及 `context_conflict`（409）、`context_not_found`（404）、`context_scope_stale`（409，设备授权或当前项目已变化）、`context_confirm_required`、`context_no_project`、`context_llm_missing`、`context_invalid` 和 `knowledge_*`。

### NV11 内置语音信息插件（Tauri 桌面命令，非 HTTP 路由）

`voice.builtin_plugins` 为不含密钥的配置对象，天气、农历、新闻、联网搜索与RAGFlow五项均默认关闭。秘密保存在SQLite `app_secrets` 的`voice.plugin.weather/search/metaso/rag`；仅packaged `voice-private`窗口通过`capabilities/voice-plugins.json`管理，主窗口/远程origin不能读取或修改。旧配置默认Tavily，`search_provider=tavily|metaso`选择独立host与密钥，失败不换供应商。

- `voice_plugins_status()` → `{settings,keys:{weather:boolean,search:boolean,metaso:boolean,rag:boolean}}`；不返回原始key。
- `voice_plugins_save({input:{settings,keys}})` → 同上。`keys.<kind>` 格式 `{action:"keep"|"set"|"clear",value?:string}`；默认保留，启用需要密钥的插件时须已有或本次新设密钥；配置校验失败时不得使插件生效。新闻源记录 `{id,url,kind?:"rss"|"newsnow",source_id?:string}`，旧记录缺省 `rss`；RSS 只允许 HTTPS，NewsNow 还允许显式填写的本机/LAN HTTP；两者禁止在 URL 中携带 query 或凭据。NewsNow 只访问固定 `/api/s?id=<配置来源ID>`，校验 `status=success/cache`、`id`、`items`，缺字段返回 schema 错误。RAGFlow 数据集 ID 只能由桌面保存，不能从模型输入指定。
- 内置工具名为`get_weather`、`get_lunar`、`get_news`（RSS/NewsNow）、`web_search`（Tavily/Metaso）、`search_rag`（RAGFlow）。调用前后复核voice许可、配置和凭据快照；旧列表不能绕过关闭/撤销。模型不能指定host、坐标或数据集，RAG响应也必须带白名单内dataset_id和非空document_id。原急停`call_raw`完整透传。Metaso固定官方SDK普通搜索契约，独立密钥、非流式，仅安全公开引用，不抓链接或上传文件。
- 成功回`{ok:true,source,observed_at,data,truncated}`；失败回`{ok:false,error:{code,message,retryable}}`，不含原始错误正文/凭据。连接3秒、总计10秒、禁止跳转和自动重试、响应≤1MiB，JSON转义后外层工具文字≤2000字符。401/403鉴权失败、429限流、432/433额度耗尽`plugin_quota_exceeded`且不可重试；超时/坏schema分别固定码。天气/新闻最多32成功条目、5分钟内存缓存，键隔离配置/凭据/参数且同键去重；Tavily/Metaso/RAG不缓存。六家本机TLS合成契约与React假桥已验证，真实账号/限额/数据质量/真人设备仍unverified，见[NV11样本依据](../../crates/moss-voice-server/tests/fixtures/nv11/README.md)。

### NV12 视觉图片 HTTP 接口（内置语音 8003，默认禁用）

只有独立视觉设置启用、模型地址与密钥齐备时，才允许已授权设备通过 MCP initialize 收到 capabilities.vision={url,token}；地址使用实际监听端口。默认不启用电脑或设备摄像头；是否主动采集图片完全由设备自身固件负责。不会把图像写入对话历史、文件或日志。

HTTP POST /mcp/vision/explain，Header 必须包含 Authorization: Bearer <设备 token>、Device-Id、Client-Id；multipart/form-data 仅允许 question（UTF-8，1..2000字）和一个图片字段，顺序任意但不能重复。图片字段名上游 `xiaozhi-esp32` 固件用的是 `file`（`filename="camera.jpg"`、`Content-Type: image/jpeg`），本服务两种名字 `image` / `file` 都接受（2026-10-16 对照固件源码后补的兼容）；其他字段名一律 422。最多 5MiB 图片，body 最多 5MiB+64KiB；MIME/魔数匹配，实际解码尺寸最大 4096×4096、总像素≤1600万。先在 middleware 验签，再由后台只读 DeviceRegistry.authorize_http 校验当前允许状态、匹配 client 和授权代次，不调用会改 last_seen 的 seen；返回前按同一代次复核，撤销再允许也不复活在途请求。全局并发2、同设备1、满额不排队；模型最长30秒，响应最大1MiB，文本最多2000字，不重定向。

成功：HTTP 200，JSON {success:true,text}。失败：{success:false,error:{code,message}}；401 无效 token，403 拒绝/已撤销/关闭，413 超大，415 格式不匹配，422 其他图片/请求错误，429 已满（Retry-After:1），502 上游异常，504 超时。所有响应 Cache-Control:no-store。视觉模型结果属于不可信资料，不赋予任何设备控制权限。

桌面仅本地 main 窗口具有 voice_vision_status() / voice_vision_save({input:{settings:{enabled,url,model,use_dialog_llm,keep_in_history,local_model},key:{action:keep|set|clear,value?}}}) / voice_vision_test() 权限。`use_dialog_llm`（2026-10-16，默认 false）打开后改用当前对话大模型看图：凭据取 `builtin_models.llm` 的地址 / 模型名与 `voice.llm.main` 密钥，`url` / `model` 与 `voice.vision.main` 不再使用；保存时要求对话模型是 OpenAI 兼容且已存密钥，否则 `vision_invalid`。`keep_in_history`（2026-10-16，默认 false）打开后，视觉请求成功的那一张照片会暂存在服务端内存 `vision::VisionPhotos`（按设备只留最新一张、最多 8 台设备、单张 data URL ≤ 4 MiB），该设备下一轮语音开始时取走并作为一条多模态用户消息接进对话历史，用于后续追问；图片只在内存、不落盘、不进日志与事件，历史按长度预算截断（一张图按 1500 token 计）。要求当前对话模型支持看图，否则每轮请求会失败。`local_model`（n74，2026-11-08，默认空）为本地视觉模型编号（`qwen3_vl_2b` / `qwen3_vl_4b`），非空且未开 `use_dialog_llm` 时照片交给本机 `llama-server`，不用地址与密钥；保存时未知编号或未下载返回 `vision_invalid`；`voice_vision_test` 会等本地模型加载完再测，加载失败返回 502 `vision_local_failed`。状态返回 settings、key_configured、ready、`local_models[]{id,label,size,installed}`、`local_runtime{state: idle/starting/ready/failed, model, accelerator, error}` 与 `dialog_llm{configured,url,model}`，不返回任何原始密钥；保存默认不开启视觉。测试按钮只使用程序在内存生成的测试 PNG 向当前生效的模型发起请求（独立视觉模型或对话模型），可能产生费用，但不会使用摄像头。此外（2026-11-07 左侧栏「摄像头」页），受信任主窗口 `voice_device_request` 支持 `capture_photo`（仅已允许设备；签发 15s 一次性 128 位随机票据并按需补发 `capabilities.vision` 后调用设备 `self.camera.take_photo`；命中票据的上传直接存入内存 `CameraPreviewStore` 并返回 `200 no-store`，不调外部视觉模型，伪造/重放/无票据仍 `403`）、`camera_frame({after_seq?})`（返回 `{running,online,session_state,has_camera_tool,unchanged,pending,last_error,error_seq,frame_seq,frame}`；命中 `after_seq` 时返回 `unchanged:true` 省略 `frame` 大图字节；非允许设备立即清帧并返回空态）与 `clear_camera_frame`（撤销设备或停服时同步清除）。Android 命令仅返回不支持；真实固件、真实模型与实机延迟留 FINAL 验收。

### NV13 Claude Code / Codex 专用 AgentRun（2026-10-15 起可开启）

AgentRun 是独立 ApprovalKind，和原有 ExternalTool、激光动作分开。语音调用只有已授权设备及有效 voice session 可以准备任务；模型不能通过参数指定 executable、工作目录、shell、凭据、MCP 配置或者 confirmed。任务为最多 4000 字符的 task，全部经 stdin 传递，不由 cmd/PowerShell 解释。argv 由实现固定：Codex 为 `exec --sandbox workspace-write --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --cd <工作目录> --json --color never -`；Claude Code 为 `--print --restricted --permission-mode acceptEdits --permission-prompts none --tools Read,Glob,Grep,Edit,Write --strict-mcp-config --mcp-config {"mcpServers":{}} --setting-sources "" --no-session-persistence --output-format stream-json --verbose`（不开放 Bash 与网络工具）。CLI 版本不再锁死，但 `--version`/`--help` 必须能识别且列出全部安全参数，否则返回 `agent_capability_missing`；探测按可执行文件的大小与修改时间缓存，文件变了会重新探测。

审批摘要绑定 provider、工作目录、executable、CLI 版本、argv/配置版本和任务；批准时按当前设置重新计算，不一致拒绝。审批卡的 agent 节点只展示任务、provider、工作目录名、版本和费用提醒，不返回目录全路径、密钥或内部 request；批准时仍须勾选确认，Agent 的确认文案是“我看过这个任务，同意在本机工作目录里运行 Agent（可能产生模型费用）”，卡片标题为“运行本机 Agent（Claude Code / Codex）”，不是激光机的现场确认文案。ApprovalService.consume_agent_run 是专用事务，验证 voice permit、数据库设备 allowed、摘要、期限和批准状态后单次消费，连同 run 记录和审计原子写入；另一条路径不能把 AgentRun 当普通 ExternalTool 或激光动作执行。

门禁：`prepare` 要求 `ApiService::strict_mode()`（`.env` 的 `MOSS_FUSION_STRICT=1`），未开启返回 `agent_strict_mode_required`，工具列表里也不出现 `agent_codex` / `agent_claude_code`；非 Windows 平台没有验证过的进程树约束，返回 `agent_sandbox_unavailable`。批准后在后台线程运行，进程树受 Windows Job Object 约束，运行中可停止。

schema v8 的 voice_agent_runs 最近 50 条记录包含 run id、approval id、提供者、工作目录（桌面状态仅目录名）、摘要指纹、状态、最多 256 KiB 输出、时间和短错误码；启动后 running -> result_unknown，不能自动重新执行。

桌面 Tauri：`voice_agent_status()` 返回 `configured`（配置里的开关）、`enabled`（配置开启**且**严格模式开启）、`strict_mode`、`providers`（每个已填 CLI 现在能不能用及原因码）与 `runs`（最多 50 条，运行中的带 `stoppable`）；`voice_agent_save({input:{enabled,workspace,codex_executable,claude_executable}})` 保存本机已有路径并验证敏感目录和明确的 exe，**开启时必须有工作目录和至少一个 CLI**，否则 `agent_config_invalid`；`voice_agent_stop({runId})` 停止正在运行的任务（进程树一起结束），返回状态并带 `stopped`，任务已结束时为 false。命令只经 capabilities/voice-agent.json 授权 main 主窗口（remote 回环，命令内固定本进程后端地址）；Android 不支持。短错误码：`agent_disabled`、`agent_strict_mode_required`、`agent_path_denied`、`agent_capability_missing`、`agent_task_invalid`、`agent_run_failed`、`agent_timeout`、`agent_stopped`、`agent_permission_expired`、`agent_config_changed`、`agent_approval_consumed`、`agent_result_unknown`、`agent_unavailable`、`agent_thread_spawn_failed`（2026-10-17 新增：批准后任务线程起不来时当场把运行记录收口为 failed，不再永远停在运行中）。探测（`--version` / `--help`）的 stdout / stderr 并发读取且进程受 kill-on-close Job Object 约束。

**剩余风险（已告知用户）**：CLI 自身的沙箱在 Windows 上不能阻止读取工作目录以外的文件；工作目录必须选成可公开给模型看的目录，界面上已写明。不得声称已实现机密文件隔离。

### NV17 固件 OTA（内置语音 HTTP 路由 + 主窗口 Tauri 命令）

内置语音服务在 `POST /xiaozhi/ota/` 的回包里带 `firmware{version,url}`：只有设备已允许**且**存在用户确认过的有效推送、并且推送版本比设备上报版本新时才给出，否则 `firmware` 为空（`ServerConfig::firmware` 为 `None` 时也一律为空）。`url` 是 `http://<OTA 回包 Host 或本机地址>:<HTTP 端口>/xiaozhi/firmware/<token>.bin`。

`GET /xiaozhi/firmware/{token}.bin`：令牌必须是 32 位十六进制（其他形式、包含 `../` 等一律 404）；令牌对应一条未取消、未到期、设备尚未上报新版本的推送；请求方地址必须等于这台设备最近一次 OTA 请求的来源地址，设备当前必须是已允许状态；发送前重新读文件并核对 SHA-256，不一致不发。第一次下载会把记录标为“下载中”。成功返回 `application/octet-stream`，其余情况一律 404（不区分原因，避免探测）。下载本身不代表刷写成功。

桌面 Tauri 命令只在 MOSS 主窗口可用（n67，2026-11-07 起独立安全窗口已取消）：capability `voice-private-main.json`（`windows:["main"]` + 回环 remote）授权，处理函数开头再用 `require_private_settings_window` 把来源固定为本进程后端上的主窗口；Android 返回仅桌面：

- `voice_firmware_status()`：返回 `enabled`、`artifacts`（已导入固件：sha256/version/project/size/created_at）、`devices`（已允许设备的型号与当前版本）、`records`（推送记录：delivery_id、device_id、firmware_version、download_status、device_report_status、expires_at、revision）与 `now`、`offer_hours`。
- `voice_firmware_import()`：弹本机文件框（只筛 `.bin`，不接受前端给的路径），校验 ESP32 应用镜像头与数字版本（≤16 MB），按 SHA-256 存到运行目录 `firmware/<sha256>.bin` 并登记；文件完全重复时只登记不重写。取消选择返回 `null`，校验失败返回 `firmware_invalid`。
- `voice_firmware_manage({input})`：只接受三种 action，字段集合必须完全匹配，否则 `firmware_invalid`。`offer{device_id,sha256,confirmed:true}` 要求设备已允许、固件已导入、版本比设备当前版本新，否则分别返回 `firmware_device_invalid` / `firmware_missing` / `firmware_not_newer`；成功时生成一次性令牌，有效期 24 小时。`cancel{delivery_id}` 未开始下载的直接删除，已开始下载的保留记录并让令牌失效；已完成或已取消返回 `firmware_conflict`。`delete{sha256,confirmed:true}` 在还有有效推送时返回 `firmware_in_use`，否则删除记录与文件，返回最新的 status。
### n61 设备唤醒词（内置语音 HTTP 路由 + 主窗口 Tauri 命令）

`GET /xiaozhi/assets/{token}.bin`：令牌 32 位十六进制、对应内存里一份未过期（30 分钟）的暂存资源包，否则 404；成功返回 `application/octet-stream` 且带精确 `Content-Length`（固件据此判断分区放不放得下）。资源包由桌面经 `VoiceServer::install_wake_assets(device_id, bytes)` 暂存，同时向该设备发 `self.assets.set_download_url{url}` 与 `self.reboot`；设备当前会话未上报该 user-only 工具时直接报错、不发送。

桌面 Tauri 命令与 NV17 一样只在 MOSS 主窗口可用（capability `voice-private-main.json`，处理函数复核 `require_private_settings_window`），Android 返回仅桌面：

- n72 工具清单：`voice_save_settings` 接受 `mcp_full_tool_list`（布尔，默认 false；变更后重连接入点），`voice_status.mcp` 新增 `full_tool_list`，`mcp.tools[]` 新增 `in_compact`（是否在精简清单里）。接入点 `tools/list` 与内置语音默认只列精简清单（`tests/golden/mcp_tools_compact.json` 的 8 个激光工具 + 电脑控制类工具），工具名、执行与门禁不变。
- n71 常连模式设置：`voice_status.settings` 新增 `builtin_mqtt_enabled`（默认 false）、`builtin_mqtt_port`（默认 1883）、`builtin_udp_port`（默认 8888），经 `voice_save_settings` 保存：开关须为布尔值，端口 1～65535，MQTT 端口不能为 8883、不能与 `builtin_ws_port` / `builtin_http_port` 相同，否则 `voice_settings_invalid`。`voice_builtin_status` 新增 `mqtt_port` / `udp_port`（实际监听的端口，未开或没开成为 `null`），`sessions[].transport` 为 `websocket` 或 `mqtt`。OTA 回复只对已允许的设备、在常连模式开成时附加 `mqtt{endpoint,client_id,username,password,publish_topic,keepalive}`（用户名为设备编号，密码为设备 token）。
- `voice_wakeword_status()` → `{word:{text,command,threshold}, base:{files,size,chip_model,text_font,emoji_count,wake_word}|null, devices:[{device_id,name,supported,screen_pack_size,screen_pack,screen_pack_state,screen_pack_pending_with_pack,wake_target}], running}`；`devices` 只含在线且已允许的设备，`supported` 表示固件上报了 `self.assets.set_download_url`；n70 起 `screen_pack_size` 为彩屏上报的 `[宽,高]`（单色屏或没报分辨率为 `null`），`screen_pack` 表示这台已刷 MOSS 全屏画风（来自设备偏好）。n77（2026-11-09）起以设备整包下载完为准：`screen_pack_state` 为 `none` / `pending_download`（已推送、等设备下载）/ `download_expired`（推送超过 30 分钟仍没下载完）/ `installed`（装的正是当前画风）/ `style_outdated`（n79：装的是换画风之前的画风包，要重刷开机待命画面才换；`screen_pack_style` 为已装的画风）；`screen_pack_pending_with_pack` 表示等下载的这次是否带画风（不带画风的包下载完会清掉已装标记）；`wake_target` 为最近一次推送的唤醒词 `custom` / `default`，没推过为 `null`。资源包下载 `GET /xiaozhi/assets/{token}.bin` 改为 64 KB 分块下发、仍带准确 `Content-Length`。
- `voice_wakeword_import_base()`：弹本机文件框（只筛 `.bin`），校验资源包格式、校验和与 `index.json`（≤8 MB），存为运行目录 `wakeword/base-assets.bin`；取消返回 `null`，失败 `wakeword_invalid` 且保留旧文件。
- `voice_wakeword_manage({input})`：字段集合必须完全匹配。`save{text,command,threshold}`（显示文字 1～16 字、拼音 2～6 个 `mn6_cn` 词表音节、阈值 0.05～0.95）；`clear_base{confirmed:true}`；`push{device_id,target:"custom"|"default",confirmed:true,screen_pack?:bool}` 要求设备已允许（`wakeword_device_invalid`）、服务运行中（`voice_builtin_stopped`）、设备在线且支持（`wakeword_device_unsupported`），返回状态加 `pushed{device_id,size,target,screen_pack}`；n70 `screen_pack:true` 时按设备分辨率与当前画风把全屏画风一起打进资源包（设备没报彩屏分辨率返回 `screen_pack_unsupported`），推送后把 `builtin_device_prefs.<设备>.screen_pack` 记为 `moss-screen-1:<画风>`，不带画风推送时清掉。

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

**并发连接名额与回环预留（2026-11-05）**：真实 TCP 入口同时最多 64 条连接。鉴权要等读完请求才能做，所以名额在接入时按对端地址分配：非回环连接最多占 56 条，最后 8 条只留给回环连接（桌面界面、Tauri 内嵌页走 127.0.0.1）。局域网慢连接或未鉴权连接占满后，本机界面的 `/api/safe-action` 急停等请求仍能接入；名额不足时直接回 503 `{"success":false,"result":"并发连接过多，请稍后重试"}`，不读请求、不进路由。实现：`try_admit_http_connection`（`MAX_CONCURRENT_HTTP`、`LOOPBACK_RESERVED_HTTP`）。

## 2. GET 路由

| 路径 | 参数 | 成功 `result` | 失败 |
|---|---|---|---|
| `/api/health` | 无 | `{"status":"ok"}` | 无 |
| `/api/system/info` | 无 | `{version, auto_check_update, check_prerelease, actual_port, preferred_port, is_fallback, web_host, mcp:{configured, connected, endpoint(token 脱敏，未配置为空串), client_info, last_request, last_request_time, messages_count, tools_count, protocol_version, recent_events[], endpoints[], core_tool, safety_gate, estop_channel}, voice:{mode, device_sessions, phase, level}}`。`auto_check_update` 未在 `settings.json` 显式配置时默认为 `true`（2026-11-07），`check_prerelease` 默认为 `false`。`configured` 只表示填了接入点地址；`connected` 表示桥当前与接入点保持连接；`client_info`、`last_request`、`recent_events` 无真实数据时为空，`messages_count` 为真实收到的帧数（无则 0），`protocol_version` 为最近一次 `initialize` 握手里 MOSS 实际应答的 MCP 协议版本、还没握手时为空串（2026-11-07 n68，原先写死 `2024-11-05`；控制台显示“未知（尚未握手）”），`core_tool`、`safety_gate`、`estop_channel` 是固定的设计说明、不是检测结果，`tools_count` 为当前开关下实际暴露的工具数（安全 19 / 开发者 20，再减去严格模式不开放的与用户关闭的工具）；`endpoints[]`（NV8）为每个接入点的 `{id, endpoint(脱敏), connected, connections, last_error}`，`id` 为 `primary` 或 `extra-N`，`connected` 为任一接入点已连接。`voice.mode` 为 `official` / `self_hosted` / `builtin`（取自运行目录 `settings.json`）；`device_sessions` 在自建链路下来自接入点健康检查，在内置链路下来自内置语音服务（未运行为 `null`），官方链路为 `null`；`phase`（`idle` / `listening` / `thinking` / `speaking` / `null`）与 `level`（`0..=100` 归一化 RMS 振幅）仅在 `builtin` 链路有会话时由内置语音服务实时写入（不含对话原文或音频样本），其他链路为 `null` / `0` | 无；非回环也免鉴权 |
| `/api/ui/runtime-config` | 无 | `{connection:{default_mode, preferred_modes:[默认, 另一], serial:{default_port:"", baudrate:115200}, network:{default_host:"", transport:"telnet", http_port:80, telnet_port:23, timeout:5.0}}, upload:{gcode_suffixes:[".gcode",".nc"]}}` | 无 |
| `/api/ui/serial-ports` | 无 | `{ports:[…], default_port, baudrate}`；只列系统真实识别到的串口 | 无 |
| `/api/ui/discover-controllers`（**POST**，body `{}`；用 POST 防止跨站页面用无 Origin 的 GET 触发扫描） | 无 | `{controllers:[{ip, telnet_port, hostname, http_port, source: mdns/scan/mdns+scan, firmware, state, machine}], busy:[{ip, telnet_port, workflow_id}], checked, not_controllers, hint}`。2026-11-09 起：mDNS（`_telnet._tcp` / `_http._tcp`，约 1.5 秒）与本机局域网网卡所在 /24 的 23 端口扫描（枚举网卡、跳过回环与代理 / VPN 点对点虚拟网卡）只产生候选，逐台连接后发实时状态查询 `?` 与 `$I`（均只读，不发运动、出光或改设置的命令），回了状态报告、`[VER:…]` 或 GRBL 启动横幅的才列入 `controllers`（`machine` 为 FluidNC `$I` 的 `[MSG:Machine: …]`）；候选一出现就开始确认（mDNS 1 秒窗口内边找边验，回了别的协议立即判定、0.7 秒无回应即放弃），通常约 1 秒返回；正在加工或待确认停机的目标不另开连接，列入 `busy`；连不上或不是 GRBL 的只计入 `not_controllers`。没有结果时 `hint` 写明扫了哪个网段。候选来源可由 `ApiService::with_controller_candidates` 替换（测试不扫真实网络）。 |
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
| mode | string | 否 | `raster`（默认，按 0.1 mm 点距逐行扫描）或 `outline`（沿深色区域描边，`trace_grayscale` + 路径优化 + `write_outline`）；其他值按 `raster` |
| width_mm、height_mm | 可选正数 | 否 | 成品尺寸（2026-10-14 起生效，此前被忽略）：都填时锁定比例放进该框；只填一边按比例算另一边；缺省、空串、null 或 0 按导出图每像素 0.1 mm。2026-11-06 起按机器幅面（默认 100 × 100 mm，可用 `work_area_width_mm` / `work_area_height_mm` 覆盖；光栅模式宽度另留两侧各 1 mm 过扫描）而不是 400 mm 画布：填写的尺寸超出返回 400；没填且按 0.1 mm/像素超出时等比缩小并返回 `auto_fitted=true` 与 `fit_note` |
| vector_simplify_factor | 可选数字 | 否 | 仅 `outline`：0.25 ~ 8，默认 3 |
| 连接字段 | 见 §1.5 | 否 | 只记录，不连接设备 |

后端直接生成光栅或轮廓 G-code（`generate_draw_artifacts`），写入运行目录 `generated_assets/`，创建 `source_type="draw"` 的 workflow 并冻结 `confirmation_snapshot`。
成功 `result` 含 `workflow_id`、`status=preview_ready`、`gcode_file`、`gcode_sha256`、`image_file`、`time_estimate`、`artifacts.width_mm/height_mm/mode`（实际成品尺寸与生成方式）、`draw_lab:{job_summary, content_fingerprint, uploaded_image_file, scene_file}` 与 §1.4 的 `web`。`outline` 没有可描边的深色图形时 400。
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

**总是 `success:true`**。`result`：`{preferred_mode, selected_mode|null, fallback_used, attempts:[{mode, target, connected, summary, laser_mode_ok, firmware_settings(仅 include_detail), timeout_seconds}], serial_connected, network_connected, summary, machine_limits:{x_travel_mm, y_travel_mm, z_travel_mm}|null, features:{speed_map, has_air_assist, has_door}|null}`。`machine_limits` 来自 FluidNC `$CD` 的 `max_travel_mm` 或经典 GRBL 的 `$130/$131`；前端用它约束画板范围。只有参数类型错或模式非法才 400。检查会真实握手（`$$` 到 `ok`、要求 `$32=1`），不发送运动或出光指令。n73：桌面、本机网页、局域网配对调用且握手成功时，后端记住这个连接（`settings.json` 的 `laser_verified_connection`），接入点与内置语音调用激光工具没给任何连接参数时用它；语音 / 接入点身份做的检查不记。

### 3.4 `/api/text-task/generate`

| 字段 | 类型 | 必填 | 默认 |
|---|---|---|---|
| text、material | string | 是 | `text 不能为空` |
| thickness_mm | float | 是 | >0，否则 400 `thickness_mm 必须是大于 0 的数字` |
| engraving_mode | string | 否 | 只用于判断是否切割（`cut`）；不再决定文字刻法（此前写 `raster` 也只描轮廓） |
| text_fill | string | 否 | `outline`（默认，2026-11-06 用户决定）只描字的轮廓，取材料库 outline 参数；`fill` 把字形按 `pixel_size_mm` 转黑白位图逐行扫描填满，取 raster 参数；`hatch` 矢量平行线填满并描边，取 outline 参数。填充按非零环绕规则，字内孔不填 |
| text_align | string | 否 | `center`（默认）/ `left` / `right`；文字里的换行即分行 |
| origin | string | 否 | `center`（默认，用户决定）开刻前激光头对准木牌（图案）中心；`lower_left` 对准木牌左下角（无木牌为文字左下角）。只改开头 `G92` 声明的激光头位置，结束时关光回到起点 |
| width_mm / height_mm（别名 board_width_mm / board_height_mm） | float | 否 | 木牌尺寸，可只给一边；没给 `font_size_mm` 时自动取能放下的最大字号并居中，给了字号但放不下 400 `…放不进 … 的木牌` |
| safe_margin_mm（别名 margin_mm） | float | 否 | 2；放进木牌时四周留边 |
| pixel_size_mm | float | 否 | 材料库对应参数（0.05～1）；`fill` 的扫描行距与 `hatch` 的排线间距 |
| font_size_mm | float | 否 | 10.0；给了木牌尺寸且没填时自动适配 |
| letter_spacing_mm | float | 否 | 0.5 |
| font_path | string | 否 | 缺省使用内置字体 |
| work_area_width_mm / work_area_height_mm | float | 否 | 100（翼宿 V1.0 幅面）；成品超出时 400 `文字成品约 W × H mm，超出机器幅面…`（2026-11-06） |
| manual_params | object | 否 | n73：这一次临时指定的 `{power_percent 1~100, feed_rate 100~6000 整数 mm/min, passes 1~5 整数}`，各项可缺省；覆盖材料库推荐或已采用的调参建议，不改材料库；有未知字段或越界 400。图片预览（`/api/workflow` preview）与画板预览同样接受 |
| 连接字段 | 见 §1.5 | 否 | 只记录 |

后端用材料库推荐参数（或 `manual_params`）生成 G-code 与预览 PNG（运行目录 `generated_assets/`），创建 `source_type="text"` 的 workflow 并冻结快照。
成功 `result` 含 `workflow_id`、`status=preview_ready`、`gcode_file`、`gcode_sha256`、`image_file`、`font_path`、`material`、`thickness_mm`、`width_mm`、`height_mm`（笔画实际范围）、`font_size_mm`（实际字号）、`text_fill`、`origin`、`origin_note`（开刻前激光头放哪）、`board_width_mm`、`board_height_mm`、`frame_mm:{min_x,max_x,min_y,max_y}`（笔画相对激光头起点的范围，供走边框）、`glyphs`、`path_optimization`、`time_estimate:{estimated_seconds, estimated_minutes, laser_seconds, non_laser_seconds}` 与 §1.4 的 `web`。
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
| `preview` / `plan` / `create` | 新记录含 `origin`（2026-11-06：发起方 `CallerContext::label`——`desktop` / `voice` / `mcp_bridge` / `gateway` / `lan` / `loopback` 等，服务端填写，请求自带的 `_origin` 被丢弃；采用反馈建议生成的新预览同样记录）。`source_type=text` 时等价于 §3.4；`source_type=image` 需要 `image_file`（已上传文件名，位于预览白名单目录）、`material`、`thickness_mm`（默认 3.0），可选（2026-11-06）`width_mm` / `height_mm`（成品尺寸，只给一边按原图比例，两边都给按比例放进该框）、`pixel_size_mm`（默认取材料库 raster 参数，0.05～1）、`image_mode`（`auto` 默认：中间灰度占比 >20% 按 Floyd–Steinberg 抖动，否则 Otsu 阈值；`dither` / `threshold`）、`threshold`（0～255，仅阈值模式）、`work_area_width_mm` / `work_area_height_mm`（默认 100）。透明像素按白色不出光；都没给尺寸时按像素间距换算原图，超出幅面（宽度另留两侧各 1 mm 过扫描）自动等比缩小并返回 `auto_fitted=true` 与 `fit_note`，显式尺寸超出直接 400。产物另含 `pixel_size_mm`、`image_mode`、`threshold`、`auto_fitted`，预览图为实际雕刻的黑白网格；`source_type=gcode` 需要 `gcode_file`。生成产物、创建记录并冻结快照 | 400 `未指定图片素材路径` / `找不到指定的素材图片文件` / `未指定 G-code 刀路路径` / `找不到指定的 G-code 刀路文件` / `图片尺寸为 0` / `成品尺寸 … 超出机器幅面` / `image_mode 只能是 …` |
| `status` | 返回记录并叠加内存中的实时 `send_result`（`running, total_lines, lines_sent, target, phase, error`） | 400 `workflow_id 不能为空`；404 `未找到 workflow` |
| `recent` | 最近记录列表 | |
| `confirm_send` / `send` | 必须 `confirmed=true`；不接受请求自带的 `start_from_line`（400 `resume_line_not_allowed`，断点行号只由 `resume` 按服务端记录的 `lines_sent` 写入，2026-11-05）；门禁顺序：记录存在 → 状态可发送（`sending/completed/failed/cancelled` 拒绝，`workflow_not_sendable`）→ 快照存在（`confirmation_snapshot_missing`）→ 物理文件 SHA-256 重算一致（`send_gate_rejected`）→ 设备连接、握手、`$CD`、预检、行校验（§1 增补）→ 同设备无在途作业（409 `device_busy`）。通过后后台逐行发送，响应 `status=sending` 与 `send_result`。可带 `offline_sd=true`（或 `send_mode=sd_offline`）走 SD 脱机模式（仅网口；固件不返回文件哈希时不执行 `$SD/Run`，任务记为 failed，2026-10-14）；SD 脱机上传端口取 `network_http_port`，其次 `http_port`，都缺省时为 80，给出的值必须是 1..=65535 的整数，否则在连接设备前返回 400 `sd_http_port_invalid`，不截断、不回落到 80（2026-11-05） | 400 / 404 / 409 |
| `resume` | 从 `start_from_line` 断点续传；同样要求 `confirmed=true`、快照与 SHA 一致，且记录未全量完成（`workflow_completed` / `workflow_not_resumable`） | 400 / 404 |
| `cancel` | 置取消并走取消序列；不承诺物理停机。有活动发送或启动预约时一律放行；无活动发送且记录（含标定矩阵）已 `completed` 时拒绝 400 `workflow_completed`，保留 completed 与逐行确认记录，提示如设备仍在运动请走急停（2026-11-05） | 400 / 404 |

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

真实驱动机器沿外接矩形走边框，用于校对材料摆放位置。可选 `relative_to_head: true`（2026-11-06，AI 预览窗“先走边框”）：先 `G92 X0 Y0` 把激光头当前位置设为原点，`min_*` / `max_*` 可为负（取预览返回的 `frame_mm`），结尾回到起点；该字段进入严格模式审批摘要。字段：`min_x`、`min_y`（默认 0）、`max_x`、`max_y`（默认 100）、`feed_rate`（默认 2000）、`power`（默认 **0，不出光**；画板勾选“弱光指示”时传 1，2026-10-14 起）与 §1.5 连接字段。`power` 必须是 0..=1000 的 JSON 整数（G-code S 标尺，下发时按设备 `$30` 换算），`feed_rate` 必须是 1..=20000 的 JSON 整数（mm/min）；超出 `i32`、越界、小数或字符串一律在审批与连接设备之前返回 400 `framing_params_invalid`，不再截断回绕（此前 `4294968096` 会变成 S800），严格模式审批摘要与实际下发因此是同一个已校验的值（2026-11-05）。流程与完整发送一致（2026-10-14 起）：生成 framing G-code → 同设备在途作业 409 `device_busy` → 打开传输 → 握手 → `$CD` → Idle 预检（失败 400 `device_preflight_failed`）→ 登记为活动作业 `framing-…`（`/api/jobs/stop-targets` 可见，顶栏急停、`/api/safe-action` 带 `job_id` 可停）→ `JobWorker::send_lines`。成功 `result`：`{message:"走边框完成", job_id, laser_on, bounds}`；失败 400（生成、连接、握手、预检、行校验），500 `framing_failed`（执行失败，已补发 0x18），409 `framing_cancelled`（被急停或取消）；后两种保留为“待确认停止”目标。不经 workflow 快照，也不需要 `confirmed`，改动时按发送链路对待。严格模式下按 R3 处理（授权规则同 §3.6）。

### 3.13 `/api/safe-action`

请求体为 `laser_contracts::LaserSafeActionArgs`：`action` 必填，可选 `confirmed`、`dry_run`、`x`、`y`、`feed_rate`、`power` 与连接字段。动作集合见 [开发者安全入口](safety.md) §2；`status` 与进给/功率倍率不需确认，其余需 `confirmed=true`。成功 200，失败 400，响应体 `{success, result, detail}`。MCP `laser_safe_action_tool` 在有宿主时转发到本路由。严格模式下，除 `status`、`emergency_stop`、`soft_reset`、`laser_off`、倍率复位与降低外的动作按 R3 处理（授权规则同 §3.6，判定函数 `laser_contracts::safe_action_requires_approval`）。

2026-10-10：HTTP 停止/查询请求可另带 `job_id`，仅限 `emergency_stop`、`soft_reset`、`status`，按登记任务使用其连接；编号未知或与显式目标不一致时失败，不改用其他设备。无编号时优先匹配显式连接目标；多个活动任务且无目标时拒绝随意选择。无活动任务且无编号时仍可通过明确的连接配置发送停止，不能依赖任务列表存在。`dry_run=true` 不写入活动任务的实时字节。成功只证明请求发送，不保证设备物理停止。

2026-11-05：`action` 先去首尾空白再转小写后匹配（与 `laser_tools::execute_laser_safe_action_with_gate`、`safe_action_requires_approval` 一致），包括 `job_id` 允许的动作检查；`Emergency_Stop`、`EMERGENCY_STOP` 在作业运行中同样走原任务通道写出实时 0x18。

### 3.14 `/api/jev/status`、`/api/jev/reveal`、`/api/jev/test`、`/api/jev/feedback`

Jev 是可选的加工反馈判断服务，只接入 OpenRouter。固定模型 `typesafe/jev-1.13`，固定 `POST https://openrouter.ai/api/alpha/decisions`，Bearer 认证；请求含 `model`、`state`、`questions.feedback_type.{type:"choice",instructions,criteria}`，响应读取 `answers.feedback_type.{type,choice,confidence}`。不使用 Chat Completions 或 TypeSafe 直连接口，不跟随重定向，超时 5 秒，响应上限 64 KiB。协议依据 OpenRouter 官方 `openapi.json`（2026-09-22 核对）。

`GET /api/jev/status` 返回 `enabled`、固定 `endpoint`、`model`、`provider`、`configured` 和 `masked_key`，不返回完整 Key。`POST /api/jev/reveal` 返回完整 Key（无 Key 为 404；桌面会话启用时要求 `X-Moss-Desktop-Session`，非桌面回环进程与局域网均拒绝）。`POST /api/jev/test` 使用已保存的 OpenRouter Key 测试 Decisions 契约，不连接设备。旧 TypeSafe Key 不能用于 OpenRouter，升级后需在设置中替换为 OpenRouter Key。`POST /api/feedback` 支持读取 `voice.builtin_local.jev_model`（含本地 Laya 或指定 OpenRouter 模型），并在未单独保存 `jev_api_key` 且内置语音大模型配置为 `https://openrouter.ai` 时安全复用 `voice.llm.main` 密钥。

`POST /api/feedback`（兼容别名 `/api/jev/feedback`）接收 `{workflow_id|task_id, feedback_text}`。必须关联已有 workflow，反馈长度为 1..2000 字符。原参数读取 `artifacts.actual_params`；无此快照的旧任务或上传 G-code 必须显式提供 `laser_max_power`、`feed_rate`、`passes`、`power_scale`，只提供建议，不能自动采用。参数须为有限数字：`0<power<=power_scale`、标尺 1..100000、速度 100..6000 mm/min、次数为 1..5 整数。新生成预览使用 1000 标尺，反馈参数中的功率、速度须为整数；超限或缺失返回 400，显式参数与实际快照冲突返回 409，不猜默认参数。

分类：`too_dark/too_light/not_cut_through/rough_edge/broken_line/acceptable/unknown`。模型只分类；本地确定性规则转换为有界参数。置信度低于 0.8 时按 `unknown` 处理（保守策略值，未经实机校准）。`acceptable/unknown` 不调整。服务未启用、缺 Key、HTTP/协议错误时由 API 统一回退本地规则；否定、多个问题及不明确反馈不调整。MCP 与 Web/桌面共用该结果，不再透传模型原始 JSON。

成功 `result` 包含 `feedback_id/workflow_id/feedback_text/created_at/source/fallback_reason/feedback_type/decision/original_params/suggested_params/adopted_params/adopted_workflow_id/can_adopt/speech`。`source` 为 `openrouter`、`local_jev`（本机 OneJev / Laya 网关分类，`decision.model` 为本地模型编号，2026-11-07）或 `local_rules`。首次仅保存建议，`adopted_params=null`；不保存 Key、原始网络响应、文件路径到模型输入。历史放在 workflow payload 的 `feedback_history`，通过 RuntimeStore 原子合并，保留当前作业状态；`GET /api/feedback/history?workflow_id=...` 读取历史。`jobs` 表仍无新增写入方，不伪造独立作业。

`POST /api/feedback/accept` 接收 `{workflow_id,feedback_id,confirmed:true}`，生成独立 `preview_ready` workflow，返回更新后的反馈记录及 `adopted_workflow_id`。成功保存新预览后才记采用参数，重复请求返回已采用记录。不会改原文件、直接修改材料库或发送硬件。新文件实际使用建议的功率、速度和次数，并重新冻结 SHA；发送仍须对新 workflow 独立 `confirm_send` 和全部既有门禁。过期或加工中的任务返回 409。MCP 采用通过现有 `regenerate_text_laser_task_tool(task_id,feedback_id,confirmed=true)`，不新增工具。

新 workflow 与原反馈的采用状态在同一 SQLite 事务（内存模式同一把锁）提交；失败不会留下可查询的孤立任务。普通 workflow 状态写回保留数据库中现有反馈历史，避免旧快照覆盖历史。新任务另有 `feedback_origin`，界面切换后仍可显示本次来源反馈。`feedback_params` 是内部生成参数，公开 text/draw/workflow 请求不能直接传入，必须走采用接口。画板当前实际生成策略仍是光栅，摘要按实际文件显示，不把输入的 outline/cut 标签当成已实现的生成策略。

### 3.15 `/api/settings` 与 `/api/system/check-update`

`{web_port, mcp_endpoint, jev_enabled, jev_api_key, jev_delete_api_key, lan_access, auto_check_update, check_prerelease}`。桌面会话启用时要求 `X-Moss-Desktop-Session`（独立 `moss-host` 与 Android 维持回环可写，局域网始终 403 `lan_read_only`）。先校验全部字段，再加锁原子读-改-写 `settings.json`（独立临时文件 + 改名，保留其他键，坏 JSON 拒绝覆盖）；没有运行目录或写入失败返回 500 `settings_write_failed`，不再显示“已保存”（2026-10-14）。`mcp_endpoint` 保留以兼容旧前端、独立 `moss-host` 与手机端；桌面端页面改在语音中心修改（技术书 §9.5.1），由语音中心直接写 `settings.json` 并立即重连。普通 Jev 配置写入运行目录 `settings.json`；Key 仍以原有方式存储于 SQLite `app_secrets`，本次不改变存储安全方案。旧 `jev_endpoint` 字段仅接受空值或固定 OpenRouter Decisions 地址，其他地址返回 400；已保存的旧地址不参与请求路由。`jev_delete_api_key:true` 删除已保存的 Key。`POST /api/system/check-update`（仅回环；桌面会话开启时要求 `X-Moss-Desktop-Session`）接受可选 `{include_prerelease}`，读取固定 GitHub Releases API（禁用重定向、6 秒超时），返回 `{current_version, latest_version, has_update, prerelease, published_at, release_url, notes, checked_at}`。

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

认证规则（在 §1.6 来源校验之后）：来源为回环地址时所有路由免认证。非回环时，`GET /api/health`、`/v1/capabilities`、`/api/system/info`、页面与静态资源（`/`、`/draw`、`/material-lab`、`/twin`、`/assets/*`、`/vendor/*`）公开，`POST /v1/auth/pair` 免 Bearer，其余数据接口（`/api/*`、`/v1/*`、`/preview`）要求 `Authorization: Bearer <token>`，缺失返回 401 `需要 Bearer 鉴权`，无效返回 401 `Bearer 鉴权失败`。

审批请求编号（2026-10-10）：可信语音 device/session、局域网 pairing_id 或网关 session 构成调用方作用域；新记录 `request_id` 为 `scope-v1:` 哈希。相同作用域和编号仅在执行参数及摘要一致时复用，不一致返回 409 `approval_request_conflict`；超过 128 字符拒绝。旧记录保持可读；旧 McpBridge 没有独立可信身份，不按自报编号复用审批。`voice_session_revoked` 表示旧语音会话不再有调用权，重新允许设备不恢复旧会话和旧审批。

桌面会话（技术书 §5.3，2026-10-06）：Tauri 桌面壳启动时调用 `ApiService::enable_desktop_session` 生成 32 字节随机会话值，页面经命令 `get_desktop_session` 取得、只存内存（该命令只交给 `main` 窗口且页面来源等于本进程后端地址，其他窗口或来源返回错误，2026-10-13），所有 API 请求带 `X-Moss-Desktop-Session`。该头**只在回环连接上有效**；值不对、或后端没有开启会话（独立 `moss-host serve`）、或来自非回环连接时一律 403 `desktop_session_invalid`（前端重新取会话后只重试一次）。有效时调用方上下文为 `Desktop`：严格模式下可直接发送，并且是唯一能调用 `/v1/approvals*` 的身份；其他身份调用审批接口得到 403 `desktop_session_required`。

审批相关错误码：`approval_not_found` 404、`approval_expired` 410、`approval_consumed` / `approval_rejected` / `approval_not_approved` / `approval_mismatch` 409、`approval_invalid_request` / `target_required` 400、`approval_unavailable` 503；严格模式下非桌面来源的 R3 请求得到 202 `result.status = "approval_required"`（`approval_id`、`expires_at`、`summary`、`speech`），回环无凭据得到 403 `strict_mode_blocked`。

语音中心（技术书 §9.4.6，P4-07）**不是 HTTP 接口**：以 Tauri 命令提供，只能由 MOSS 桌面窗口调用；改写设置、启停进程、测试/保存模型、读日志、本地模型下载删除、设备允许与改名，以及视觉/音乐/Agent 全部命令，另要求调用窗口为 `main` 且来源等于本进程后端地址，否则返回 `voice_acl_denied`（2026-10-13）；只读的 `voice_status`、`voice_mode_changes`、`voice_builtin_status` 不受此限；HTTP 后端（桌面内置、独立 `moss-host serve`、Android）都没有 `/v1/voice/*`，一律 404（A-68）。命令与参数：`voice_status()`、`voice_save_settings({settings})`（返回与 `voice_status` 相同的内容，另含 `reconnected`：本次保存是否重新发起了接入点连接且有桥启动；地址未变、只改健康检查地址、自建模式下改官方备份或没有可连接的接入点时为 `false`；握手是否成功看 `bridge.connected`，2026-10-14）、`voice_mode_changes({mode})`、`voice_set_mode({mode})`（`official` / `self_hosted` / `builtin`）、`voice_server_start()` / `voice_server_stop()` / `voice_server_restart()`、`voice_server_log({tail})`、`voice_models_test({section, input})`（`llm` / `asr` / `tts`；`input` 含密钥，只在本次调用内存中使用）、`voice_models_save({input})`；`exit_application({stopServer})`。接入点与工具（NV8）：`voice_save_settings` 另接受 `add_extra_mcp_endpoint`（附加官方接入点，`ws://` / `wss://`、不含占位符、不能重复、最多 4 个）、`remove_extra_mcp_endpoint`（编号 `extra-N`）、`disabled_tools`（关闭的工具名数组，整体替换；未知工具或 `laser_safe_action_tool` 返回 400 `voice_settings_invalid`），写入 `settings.json` 顶层 `mcp_extra_endpoints` / `mcp_disabled_tools`（列表为空时删除键）并按需重连小智桥；`voice_status` 另含 `mcp`：`{endpoints[]: {id, endpoint(脱敏), active, connected, connected_at, connections, last_error, client, identity{fields, issued_at, expires_at, expired} | null}, extras_paused, max_extra_endpoints, tools[]: {name, description, enabled, can_disable, voice_hidden, calls, failures, denied, last_at, last_caller}}`。被关闭的工具经接入点或内置语音调用时返回工具错误 `tool_disabled`。内置语音服务（NV1）：`voice_builtin_status()`、`voice_builtin_start()`、`voice_builtin_stop()`、`voice_device_set_status({deviceId, status})`（`allowed` / `revoked`）、`voice_device_rename({deviceId, name})`、`voice_builtin_models_test({section, input})`（`llm` 结论附分段耗时与思考提示，快速回答开启时带关闭思考参数测试，返回可含 `thinking_off`：`applied` / `unsupported`，n73；密钥留空时用当前分区或同源 OpenAI 兼容分区已保存的密钥；大模型使用流式 `stream: true` 探针，收到首个 `tool_calls` 分片即判定成功返回，同时兼容非流式 JSON；语音合成测试通过时另在 `audio_data_url` 返回合成音频 Data URL 供前端试听；识别、合成的 `input.kind` 可为 `doubao`——豆包流式，地址为 `wss://`、`model` 为资源 ID，二者留空用默认值，缺省 `openai`；外部服务端的 `voice_models_*` 不接受 `doubao`）、`voice_builtin_models_list({section, input})`（共享 Keep-Alive 连接池，5 秒连接超时 / 8 秒列表超时，读取该服务商 `<基础地址>/models` 的全量模型 ID 列表 `models[]` 与原生类型分类映射 `model_sections`，不要求先填模型名与音色，密钥留空时沿用当前或同源 OpenAI 兼容分区已保存密钥，结果与报错不含密钥；`doubao` 直接返回内置常用资源 ID；`llm` 指向 `api.cline.bot` 时置顶补齐 `cline-pass/*` 订阅模型并在 `/models` 返回 404 时回退列出订阅模型；OpenAI 兼容测试/对话/视觉对 `api.cline.bot` 附带 `X-Response-Format: openai` 与 Cline 客户端头并兼容解包 `{data:{choices}}`）、`voice_builtin_models_save({input, clear})`（`clear` 为要删除的分区；密钥留空时支持继承同源分区已存密钥；运行中会重启内置服务，返回 `restarted`）；本地模型（NV6）：`voice_local_save({settings})`（`llm_fast_reply`：快速回答，默认 `true`，对认识的服务商在大模型请求中附带关闭思考参数，服务不认时自动去掉重试，n73，2026-11-08；`asr` / `tts` / `vad` 开关、`vad_model` / `asr_model` / `tts_model`（选用的模型 id，须存在且类别相符，否则 400 `voice_settings_invalid`）、`tts_speaker`、`tts_speed`、`mirror`；运行中会重启内置服务）、`voice_local_model_download({id})` / `voice_local_model_cancel({id})` / `voice_local_model_delete({id})`（id：判停 `silero_vad` / `ten_vad`，识别 `sense_voice` / `sense_voice_2025` / `x_asr_streaming`，合成 `kokoro` / `kokoro_v11` / `matcha_zh_en`；运行中删除返回 409 `local_model_in_use`；多个模型可同时下载，同一模型重复下载返回 409；删除先把模型整体改名到 `.trash-*` 再删，文件被占用时返回 500 `local_model_delete_failed` 且模型原样保留；删掉同目录最后一档 OneJev 时一并删除自动下载的推理引擎 `<models>/.runtime/llama-*`（用户自备的 `<models>/llama` 不动）；删除 `laya_multilingual` 时取消进行中的环境准备，并删除带 `.moss-managed` 标记的 MOSS 自建虚拟环境，2026-11-07）、`voice_local_laya_setup({action})`（`start` / `cancel`：用本机 Python 3.10+ 在 Laya 根目录创建 `.venv` 并安装 `laya[serve]`，有 NVIDIA 驱动时先装对应 CUDA 版 PyTorch；本机没有可用 Python 或设置了 `MOSS_LAYA_PYTHON` 时返回 412 `laya_env_missing` 并说明原因；下载 `laya_multilingual` 且运行环境未就绪时后端自动开始准备，`laya_multilingual:force` 只下载权重，2026-11-07）；`voice_builtin_status` 另含 `local`（设置、`models[]` 的 `id`、`kind`（`vad` / `asr` / `tts`）、`engine`、`label`、`note`（优缺点）、`size`（含附加文件）、`streaming`、`voices{default, max, hint}`（仅多音色合成模型）、`installed` 与 `progress{state: downloading/preparing_runtime/waiting_runtime/verifying/installing/failed/cancelled}`（`preparing_runtime{done,total}`：OneJev 正在下载共用推理引擎；`waiting_runtime`：另一档正在安装推理引擎，排队中，可取消）、`laya_env`（另含 `managed`、`venv_dir`）、`laya_setup{state: idle/running/done/failed/cancelled, stage, step, steps, detail, elapsed_secs, summary, accelerator, message, log[]}`、`jev_runtime{model, state: cloud/not_installed/idle/stopped/starting/ready/failed, accelerator, error, notice}`（所选本地 Jev 的实际运行情况：`failed` 时 `error` 为后台启动失败或推理进程意外退出的原因，可能附推理进程最后几行错误输出；`notice` 为 9B 自动关闭“没说完”判断、无显卡加速时 4B / 9B 偏慢等提示，2026-11-07）、`helper_found`、`helper_running`、`ready`）。`voice_builtin_status` 返回 `mode`（`chat` / `echo`）、`link_mode`、`models`（每个分区的 `configured`、`key_set`、`kind`、地址与模型，**不含密钥**）、`last_error`；错误码另有 `builtin_start_failed`（409，端口被占用等）、`voice_device_not_found`（404）、`voice_device_invalid`（400）。内置服务对设备开放的端口只提供小智协议：`GET/POST /xiaozhi/ota/`（OTA 端口，默认 8003）与任意路径的 WebSocket（设备端口，默认 8000），见内置语音技术书 §13。失败统一为 `{code, message, status}`，常见 `code`：`server_already_running` / `server_external` / `server_not_running` / `server_not_owned`（409）、`voice_config_invalid` / `server_config_missing` / `voice_settings_invalid` / `model_input_invalid`（400）、`self_hosted_endpoint_missing`、`model_test_required`（409，保存内容与测试通过的不一致）、`server_full_module`（409，全模块部署只测不写）、`voice_mode_switch_failed`（已回滚）、`voice_desktop_only`（移动端）。返回内容中带令牌的地址均已脱敏，不含任何模型密钥。

配对语义（`AuthManager`）：配对码按来源 IP 限流，连续错误 5 次锁定 15 分钟，锁定时返回 429 `配对请求过于频繁，请稍后重试` 并带 `Retry-After`；配对码无效、已使用或已过期统一返回 400 `配对码无效或已过期`，不通过文案区分原因。token 为 32 字节 CSPRNG 随机值的 Base64，服务端只保存其 SHA-256 哈希与 `pairing_id`。**全部状态在进程内存中**，`auth_pairings` 表当前不写入，宿主重启后所有配对失效；没有撤销接口。非回环部署默认仅限受信任局域网明文 HTTP，禁止公网直连；跨网必须由前置代理终结 TLS。移动端复用同一套路由，不新增独立的危险操作接口。

### 5.0 语音记录与诊断（n68，2026-11-07）

- **事件**：`voice_builtin_status.events[]` 为全部保留的事件，新的在前，每条 `{seq, at, at_ms, level, source, category, text, hint?, detail?}`：`seq` 递增序号；`source`（n76，2026-11-09）为 `xiaozhi`（小智设备、对话与模型链路）/ `laser`（激光雕刻机：握手、走边框、急停、设备操作失败、加工完成 / 停止 / 失败，经 `laser_api::ApiService::set_event_sink` 写入）/ `moss`（MOSS 软件本身：`internal` 分类、内置服务启停与启动失败、本地视觉模型进程、应用内更新失败），定义在 `moss_voice_server::DiagSource`，缺省视为 `xiaozhi`；`at` 为 Unix 秒（兼容旧界面）、`at_ms` 为 Unix 毫秒；`level` 为 `info` / `warn` / `error`；`category` 为闭集 `general`（一般流程）/ `network`（网络）/ `auth`（密钥或权限）/ `quota`（余额、额度、频率）/ `config`（地址、模型名、参数）/ `device`（小智设备）/ `firmware`（固件与资源包）/ `model`（模型服务或本地模型程序）/ `audio`（解码、判停、播放）/ `internal`（MOSS 内部），定义在 `moss_voice_providers::DiagCategory`，前端大白话映射在 `voiceState.js` 的 `VOICE_DIAG_CATEGORIES`；`text` 是大白话说明，`hint` 是下一步建议，`detail` 是原始技术信息（底层错误链、服务方原话），写入前经 `scrub_diagnostic_text` 去掉疑似密钥、`token=` 一类参数、`Bearer` 后的值、地址里的账号密码与查询串、IPv4 地址，最长 2000 字并标“已截断”。没有建议或细节时不输出这两个字段。
- **保留与覆盖**：事件与耗时存在桌面端 `BuiltinVoice` 持有的 `DiagnosticsLog`（只在内存，不落盘），作为 `ServerConfig.diagnostics` 传给每次启动的服务，**服务停止、重启、崩溃重启都不清空**，退出 MOSS 才清空。日常记录（`info`）最多 300 条，错误与警告另一个缓冲最多 200 条，日常记录挤不掉错误；耗时最多 50 轮。`events_dropped: {info, problems, timings, max_info, max_problems, max_timings}` 为已被覆盖的条数与各自上限，界面据此提示，不悄悄丢。排障原文 `debug_traces` 仍归服务实例所有（默认关闭，关闭开关或重启立即清空），每条另有 `truncated`（文字超过 240 字或工具调用超过 8 个）与 `tools_dropped`。
- **状态新增字段**：`events_dropped`；`warnings[]`（当前生效的降级提示：本地音乐目录不可用、声纹按人记忆未生效）；`timings[]` 每轮另有 `device_id` 与 `at_ms`（结束时间）；`outcome` 取值 `running` / `done` / `interrupted` / `ignored`（没识别出文字）/ `music_started` / `disconnected`。服务未运行时 `events` / `timings` 仍返回记录本内容。
- **写进记录的桌面端后台失败**（以前只打印到看不见的控制台）：本机 Agent 只读工具挂载失败、本地模型下载残留清理、结束上次遗留的 OneJev 进程、固件下载记录恢复失败、本地 Jev（OneJev / Laya）各处启动失败、内置服务启动或重启失败（含随 MOSS 自动启动）、本地音乐目录不可用、声纹按人记忆未生效。
- **提供方错误**：`ProviderError {message, category, hint, detail}`；网络错误按底层原因分开说（超时 / 域名解析失败 / TLS 证书 / 对方拒绝连接 / 网络不通 / 中途断开 / 其他）；HTTP 状态按 401、403 → `auth`，402、429 → `quota`，400、404、422、3xx → `config`，5xx → `model` 归类，服务方原话放 `detail`。对话出错记为 `error`；已自动补救的（流式识别失败改整段识别、主合成失败降级本地合成）记为 `warn`。设备异常断开说明最可能原因（Wi-Fi 掉线、设备重启或断电）。
- **命令错误**：`CenterError` 另有可选 `detail`（原始技术信息，已脱敏）；数据库失败统一为“MOSS 本机设置数据库读写失败……”，Debug 文本只进 `detail`。
- **审批列表**：`GET /v1/approvals` 结果另有 `limit`（50）。界面写“最近 N 条”，满额时注明只显示最近 50 条。
- **导出诊断包**：`voice_diagnostics_export()`（无参数；与 `voice_builtin_status` 同权限：`default` capability 授权，命令开头 `require_status_window` 固定为主窗口）。后端生成 JSON 后弹出系统“另存为”框，路径与内容都不来自网页；取消返回 `{saved:false}`，成功返回 `{saved:true, file_name, bytes, events}`（不回传完整路径），写入失败返回 `diagnostics_export_failed`。文件内容：`{format:"moss-voice-diagnostics", format_version:1, generated_at_ms, app{name, version, os, arch}, link_mode, running, mode, ports{ws, http}, models, jev, local{ready, helper_found, helper_running, models, jev_runtime, onejev_env, laya_env, laya_setup, asr_model, tts_model, vad_model}, last_error, warnings, events, events_dropped, timings, debug_enabled, debug_traces, sessions, devices[{device_id, name, model, firmware_version, status, last_seen}], approvals{limit:50, items, error}, included[], excluded[]}`。
- **Colab T4 专属联动、Colab CLI 自动部署与笔记本导出（n80，2026-11-10）**：`voice_builtin_status.local.colab` 返回 `{enabled, url, llm_model, tts_model, tts_voice, tts_speed, key_configured, active, cli:{installed, logged_in, account_hint, expires_in, auth_url, runner_state, runner_step, runner_error, ref_audio_configured, ref_audio_name}}`（密钥只存 `app_secrets` 的 `voice.colab.main`，永不回传；账号邮箱由 `mask_email` 脱敏）；`voice_local_save` 接受可选 `colab: {enabled?, url?, llm_model?, tts_model?, tts_voice?, tts_speed?, api_key?, clear_key?}` 与 `colab_cli: {action: "refresh"|"install"|"login_start"|"login_complete"|"logout"|"start_session"|"stop_session"|"set_ref_audio"|"clear_ref_audio", code?, file_name?, wav_base64?}`（已登录 `colab` CLI 时开启 `colab.enabled=true` 且地址或密钥为空会自动在后台启动 `colab` CLI 分配云端 T4、同步本机 `<runtime_root>/colab-cli/moss_ref.wav` 550W 参考音频并回填隧道地址与密钥）；`voice_builtin_models_test("colab", {target?: "all"|"llm"|"tts", ...})` 探测 Colab 隧道；`voice_colab_export_notebook()`（主窗口 `require_main_window`）由后端生成固定 `.ipynb` 并弹出系统保存框，返回 `{saved, file_name?, bytes?}`。
  - **包含**：版本与系统、链路与端口、模型服务类型 / 地址 / 模型名 / 音色与“是否填过密钥”、本地模型与 Jev 状态、全部保留的事件与覆盖计数、耗时（含会话号与设备编号）、排障开关打开时的最近 10 轮原文、在线会话与已登记设备、最近 50 条审批（含结果说明与错误码）、最近一次启动失败与降级提示。
  - **不包含**：任何密钥 / 令牌 / 密码（只记录是否填过）、录音与合成音频、排障关闭时的对话文字、对话记忆 / 长期记忆 / 资料库 / 声纹、家居地址与令牌、固件文件、设备上报的原始状态快照。
  - **脱敏**：整个 JSON 的每个字符串再经 `moss_voice::redact::redact_line` 与 `scrub_diagnostic_text`；IPv4 一律换成 `<ip>`（四段数字的版本号也会被换掉），域名与设备编号保留，便于排障。

### 5.1 第三方 MCP（NV9，2026-10-10）

桌面新增 `voice_mcp_status`、`voice_mcp_save({input})`、`voice_mcp_delete({name,revision,confirmed})`、`voice_mcp_reconnect({name,confirmed})`。只授予主窗口且在命令入口再次核对实际回环来源与桌面会话，没有 HTTP 配置／执行接口。输入、返回、凭据引用与失败语义见 [第三方 MCP 契约](../integration/moss-external-mcp.md)。

现有审批列表增加 `kind=external_tool`、`target.mode=external` 和 `external:{tool,arguments}`（仅有界脱敏预览）。批准仍只接受桌面身份，使用完整隐藏 request 和当前第三方连接／权限重算摘要；授权只消费一次，不能使用参数中的 `confirmed` 或 `approval_id` 绕过。外部审批与激光审批使用各自的执行分支，不会把外部参数变成设备命令。第三方结果不确定或结果存储失败不自动重试；连接恢复也不补发旧调用。

### 5.2 应用内更新命令（2026-11-07）

客户端新增 Tauri 命令 `app_update_status()` 与 `app_update_install({tag})`（无 HTTP 接口），入口校验受信任主窗口（桌面端 `window_is_trusted_main`，Android 端 `is_trusted_mobile_main`）。`app_update_status` 返回 `{supported, unsupported_reason, platform, phase, version, downloaded, total, error}`，其中 `platform` 为 `windows` / `macos` / `android` / `other`，`phase` 为 `idle` / `checking` / `downloading` / `installing` / `restarting`（桌面）/ `installer_launched`（Android）/ `error`。`app_update_install` 在 `ApiService::device_work_in_progress()` 为真时拒绝（下载完安装前二次复核），只从固定前缀 `https://github.com/yjdaaa/moss/releases/download/<tag>/latest.json` 拉取更新包并用内置公钥校验 minisign 签名与 `version:` 字段；桌面端按“完全退出”顺序急停并停服务后替换重启，Android 端把校验通过的 APK 暂存于应用私有缓存并经 `FileProvider` 拉起系统安装器确认升级。

## 6. 黄金样本生成要求（阶段 1）

黄金样本采集自 Python 版（`tests/golden/api/`），Rust 版不再自动差分，保留作为字段形态与错误文案的参考。掩码字段：`workflow_id`、`task_id`、`calibration_id`、`job_id`、`created_at`、`updated_at`、所有绝对路径、`content_fingerprint`、`source_path`、`backup_path`、`params_file`（Python 返回绝对路径，Rust 只返回文件名，值本身不可比）。注意样本文件本身仍含采集机器的绝对路径。材料库样本比对时，Python 侧 `null`/`{}` 墓碑节点视为该格不存在（db-schema.md §3.6.2）。比对时 `result.web.*_url` 只比路径部分。

比对方法（归档规格 §20.1 级别 1）：响应体解析为 JSON 后 deep-equal，不比文本。HTTP 状态码、`success` 值、`result`/`detail` 结构必须相等；错误文案字符串逐字相等，因为用户会看到；数值按数值比较；键序与空白忽略。`/preview` 返回的文件按 SHA-256 比较。黄金样本保存原始响应，掩码与规范化在比对时进行。
