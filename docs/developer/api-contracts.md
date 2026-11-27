# Web API 接口契约（`/api/*` 与 `/v1/*` 路由、文件与静态服务）

状态：2026-09-22 按 `crates/laser-api/src/lib.rs` 现状核对；2026-11-15 按 `routes.rs` / `server.rs` / `approval/desktop.rs` 路由与桌面 `generate_handler!` 命令清单补齐。路由表与字段以代码为准；黄金样本 `tests/golden/api/` 记录的是 Python 版响应，比对规则见 §6。
历史依据：原稿从 Python 版 `web_server.py` 提取（2026-09-14）。
用途：前端、MCP 工具转发（`laser_host::InProcessBackend` 直接调用同一进程内 `ApiService::dispatch_json` / `dispatch_json_with_watch`，与 HTTP 入口共用门禁与状态机）与测试共同遵守的 HTTP 契约。MCP 工具描述和小智提示词的自然语言调整不改变本文约定的字段、状态码和响应结构。

路由总表：

| 方法 | 路径 | 说明 | 详见 |
|---|---|---|---|
| GET | `/api/health` | 存活检查 | §2 |
| GET | `/api/system/info` | 端口、MCP 桥状态与遥测；非回环免鉴权，未鉴权时隐藏 MCP 接入点与遥测明细 | §2 |
| GET | `/api/system/lan-info` | 本机局域网地址、端口与局域网访问状态；仅回环 | §2 |
| POST | `/api/system/check-update` | 检查 GitHub 发布版更新；仅回环 | §3.15 |
| GET | `/api/jobs/stop-targets` | 顶栏急停与激光任务进度浮层的后端原始任务目标：`result.jobs` 数组与 `result.can_acknowledge_stop` 布尔值（当前请求身份是否允许调用 `/api/jobs/acknowledge-stop`）。除 `job_id`、`target`、`state` (`running` / `unknown`) 外，2026-11-02 起运行中的作业还带只读进度字段 `workflow_id`、`phase` (`streaming` / `awaiting_completion`)、`lines_sent`、`total_lines`、`elapsed_ms`、`mode`（取自 `ActiveJob::live_send_result`），2026-11-18 起另带 `paused`（走边框被暂停还没继续）。**纯只读**：不开设备连接、不写实时通道；状态未知不代表已停机，多个目标不得随机选择 | §2 |
| POST | `/api/jobs/acknowledge-stop` | 交互端操作者现场核实机器已停稳后清除指定非空 `job_id` 且 `confirmed:true` 的 `uncertain_jobs` 待确认停止记录（拒绝语音/MCP 桥/审批重放调用方，不支持无编号全量清空，不影响运行中的 `active_jobs`） | §2 |
| GET | `/api/ui/runtime-config` | 连接默认值与上传后缀 | §2 |
| GET | `/api/ui/serial-ports` | 系统串口列表 | §2 |
| POST | `/api/ui/discover-controllers` | 查找并只读识别局域网里的 GRBL / FluidNC（mDNS + 本网段 23 端口，逐台 `?` + `$I`；2026-11-09） | §2 |
| GET | `/api/ui/material-options` | 材料名与厚度 | §2 |
| GET | `/api/ui/material-recommendation` | 材料推荐参数 | §2 |
| GET | `/api/material-lab/export` | 材料库导出 | §2 |
| GET / POST | `/api/material-lab/recent` | 最近一条含完整参数的 workflow | §3.11 |
| GET | `/preview` | 受控文件服务 | §2.1 |
| GET | `/`、`/draw`、`/draw/*`、`/material-lab`、`/twin`、`/voice`、`/camera`、`/assets/*`、`/vendor/*` | 嵌入的前端 | §2.2 |
| POST | `/api/upload` | 上传图片或 G-code | §3.1 |
| POST | `/api/draw/preview` | 画板预览 | §3.2 |
| GET / POST | `/api/draw/state` | 创作画板持久化状态（仅本机桌面 / 回环可读写；非本机返回 403，退回各自浏览器 `localStorage`） | §3.2a |
| POST | `/api/connection/check` | 连接检查与设备能力 | §3.3 |
| POST | `/api/text-task/generate` | 文字任务预览 | §3.4 |
| POST | `/api/text-task/send` | 文字任务确认发送 | §3.5 |
| POST | `/api/workflow` | workflow 状态机 | §3.6 |
| POST | `/api/material-lab/preview` / `send` / `status` / `select-cell` / `manage` | 材料实验室 | §3.7～3.10 |
| GET / POST | `/api/text-origin` | 文字对刀起点全局设置（r48）：GET 读，POST `{origin:"center"\|"lower_left"}` 改；工作台与语音生成文字任务共用（局域网只读） | §3.10c |
| GET / POST | `/api/engrave-route` | 雕刻路线全局开关（矢量 / 转图片，r45）：GET 读，POST `{route:"vector"\|"image"}` 改；工作台、画板与语音生成共用（局域网只读） | §3.10c |
| GET / POST | `/api/time-calibration` | 工时自校准：GET 查看各台机器的校准系数与样本数；POST `{action:"reset", target?}` 清除（局域网只读，r40） | §3.10b |
| POST | `/api/device/framing` | 走边框 | §3.12 |
| POST | `/api/safe-action` | 受控安全动作 | §3.13 |
| POST | `/api/registration/read-position`、`/api/registration/preview` | 两点对位：只读当前位置（状态查询）、按两对参考点生成旋转平移后的新预览 | §3.13a |
| POST | `/api/device/pointer-fire` | 低功率点射（≤ S10、≤ 200 ms，`M3` 原地出光后关光），只给界面上的人用 | §3.13b |
| POST | `/api/settings` | 保存监听端口等设置 | §3.15 |
| POST | `/api/process-trace` | 完整过程记录的状态与清空（n105，仅桌面；局域网拒绝） | §3.15 |
| GET | `/api/jev/status` | Jev 配置状态（Key 只给掩码） | §3.14 |
| POST | `/api/jev/reveal`、`/api/jev/test` | 取完整 Key、测试 Jev 连接 | §3.14 |
| POST | `/api/feedback`（别名 `/api/jev/feedback`）、`/api/feedback/accept` | 加工反馈调参、采用建议参数 | §3.14 |
| GET | `/api/feedback/history` | 某 workflow 的反馈历史 | §3.14 |
| GET / POST | `/v1/approvals`、`/v1/approvals/{id}/approve`、`/v1/approvals/{id}/reject` | 桌面审批列表与批准 / 拒绝 | §5 |
| GET | `/v1/capabilities`、`/v1/events`、`/v1/material-lab/bootstrap` | 能力、事件、材料页首屏 | §5 |
| POST | `/v1/auth/challenge`、`/v1/auth/pair` | 局域网配对 | §5 |

---

### NV10 内置语音记忆（后端内部状态，管理入口见 NV19）

`voice.builtin_memory_enabled` 缺省为 `false`。旧桌面端 `voice_memory_list` / `voice_memory_set_enabled` / `voice_memory_save` / `voice_memory_clear` 命令已下线删除；界面统一通过 NV19 `voice_context_manage`（如 `set_options { sync_legacy_memory: true }`、`list_all`、`clear_all`）联动管理。

摘要生成采用已配置的大模型，可为第三方云模型，用户须主动开启并在界面获知数据将发送给其已配置的服务商。服务端仅把已提交的用户/助手文字（筛除工具消息，最多 6000 字符）交给当前 LLM，无工具权限；模型输出按 1500 字符上限和 revision 比对持久化。编辑/清空/关闭/设备撤销通过同一内存协调门禁，原始音频不入记忆数据库。真实模型隐私边界、总结质量和物理设备体验另行验收。

### NV19 长期记忆（Tauri 桌面命令，非 HTTP 路由）

桌面主窗口专用（n26 / n67 起独立安全窗口已取消）：权限在 `capabilities/voice-private-main.json`（`windows:["main"]` + 回环 `remote`），处理函数开头再用 `require_private_settings_window` 固定为本进程回环后端上的主窗口；局域网、其他本机端口、手机端均不可调用，移动端返回 `voice_desktop_only`。

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

`voice.builtin_plugins` 为不含密钥的配置对象，天气、农历、新闻、联网搜索与RAGFlow五项均默认关闭。秘密保存在SQLite `app_secrets` 的`voice.plugin.weather/search/metaso/rag`；仅经 `capabilities/voice-private-main.json` 授权且通过 `require_private_settings_window` 校验的本机主窗口可管理，局域网或其他远程来源不能读取或修改。旧配置默认Tavily，`search_provider=tavily|metaso`选择独立host与密钥，失败不换供应商。

- `voice_plugins_status()` → `{settings,keys:{weather:boolean,search:boolean,metaso:boolean,rag:boolean}}`；不返回原始key。
- `voice_plugins_save({input:{settings,keys}})` → 同上。`keys.<kind>` 格式 `{action:"keep"|"set"|"clear",value?:string}`；默认保留，启用需要密钥的插件时须已有或本次新设密钥；配置校验失败时不得使插件生效。新闻源记录 `{id,url,kind?:"rss"|"newsnow",source_id?:string}`，旧记录缺省 `rss`；RSS 只允许 HTTPS，NewsNow 还允许显式填写的本机/LAN HTTP；两者禁止在 URL 中携带 query 或凭据。NewsNow 只访问固定 `/api/s?id=<配置来源ID>`，校验 `status=success/cache`、`id`、`items`，缺字段返回 schema 错误。RAGFlow 数据集 ID 只能由桌面保存，不能从模型输入指定。
- 内置工具名为`get_weather`、`get_lunar`、`get_news`（RSS/NewsNow）、`web_search`（Tavily/Metaso）、`search_rag`（RAGFlow）。调用前后复核voice许可、配置和凭据快照；旧列表不能绕过关闭/撤销。模型不能指定host、坐标或数据集，RAG响应也必须带白名单内dataset_id和非空document_id。原急停`call_raw`完整透传。Metaso固定官方SDK普通搜索契约，独立密钥、非流式，仅安全公开引用，不抓链接或上传文件。
- 成功回`{ok:true,source,observed_at,data,truncated}`；失败回`{ok:false,error:{code,message,retryable}}`，不含原始错误正文/凭据。连接3秒、总计10秒、禁止跳转和自动重试、响应≤1MiB，JSON转义后外层工具文字≤2000字符。401/403鉴权失败、429限流、432/433额度耗尽`plugin_quota_exceeded`且不可重试；超时/坏schema分别固定码。天气/新闻最多32成功条目、5分钟内存缓存，键隔离配置/凭据/参数且同键去重；Tavily/Metaso/RAG不缓存。六家本机TLS合成契约与React假桥已验证，真实账号/限额/数据质量/真人设备仍unverified，见[NV11样本依据](../../crates/moss-voice-server/tests/fixtures/nv11/README.md)。

### NV12 视觉图片 HTTP 接口（内置语音 8003，默认禁用）

只有独立视觉设置启用、模型地址与密钥齐备时，才允许已授权设备通过 MCP initialize 收到 capabilities.vision={url,token}；地址使用实际监听端口。默认不启用电脑或设备摄像头；是否主动采集图片完全由设备自身固件负责。不会把图像写入对话历史、文件或日志。

HTTP POST /mcp/vision/explain，Header 必须包含 Authorization: Bearer <设备 token>、Device-Id、Client-Id；multipart/form-data 仅允许 question（UTF-8，1..2000字）和一个图片字段，顺序任意但不能重复。图片字段名上游 `xiaozhi-esp32` 固件用的是 `file`（`filename="camera.jpg"`、`Content-Type: image/jpeg`），本服务两种名字 `image` / `file` 都接受（2026-10-16 对照固件源码后补的兼容）；其他字段名一律 422。最多 5MiB 图片，body 最多 5MiB+64KiB；MIME/魔数匹配，实际解码尺寸最大 4096×4096、总像素≤1600万。先在 middleware 验签，再由后台只读 DeviceRegistry.authorize_http 校验当前允许状态、匹配 client 和授权代次，不调用会改 last_seen 的 seen；返回前按同一代次复核，撤销再允许也不复活在途请求。全局并发2、同设备1、满额不排队；模型最长30秒，响应最大1MiB，文本最多2000字，不重定向。

成功：HTTP 200，JSON {success:true,text}。失败：{success:false,error:{code,message}}；401 无效 token，403 拒绝/已撤销/关闭，413 超大，415 格式不匹配，422 其他图片/请求错误，429 已满（Retry-After:1），502 上游异常，504 超时。所有响应 Cache-Control:no-store。视觉模型结果属于不可信资料，不赋予任何设备控制权限。

桌面仅本地 main 窗口具有 voice_vision_status() / voice_vision_save({input:{settings:{enabled,url,model,use_dialog_llm,keep_in_history,local_model},key:{action:keep|set|clear,value?}}}) / voice_vision_test() 权限。`use_dialog_llm`（2026-10-16，默认 false）打开后改用当前对话大模型看图：凭据取 `builtin_models.llm` 的地址 / 模型名与 `voice.llm.main` 密钥，`url` / `model` 与 `voice.vision.main` 不再使用；保存时要求对话模型是 OpenAI 兼容且已存密钥，否则 `vision_invalid`。`keep_in_history`（2026-10-16，默认 false）打开后，视觉请求成功的那一张照片会暂存在服务端内存 `vision::VisionPhotos`（按设备只留最新一张、最多 8 台设备、单张 data URL ≤ 4 MiB），该设备下一轮语音开始时取走并作为一条多模态用户消息接进对话历史，用于后续追问；图片只在内存、不落盘、不进日志与事件，历史按长度预算截断（一张图按 1500 token 计）。要求当前对话模型支持看图，否则每轮请求会失败。`local_model`（n74，2026-11-08，默认空；n93 扩展新代模型）为本地视觉模型编号（`qwen3_5_2b` / `qwen3_5_4b` / `qwen3_vl_2b` / `qwen3_vl_4b`），非空且未开 `use_dialog_llm` 时照片交给本机 `llama-server`，不用地址与密钥；保存时未知编号或未下载返回 `vision_invalid`；`voice_vision_test` 会等本地模型加载完再测（4B 型号启动超时放宽至 90 秒，n93），加载失败返回 502 `vision_local_failed`。状态返回 settings、key_configured、ready、`local_models[]{id,label,size,installed}`、`local_runtime{state: idle/starting/ready/failed, model, accelerator, error}` 与 `dialog_llm{configured,url,model}`，不返回任何原始密钥；保存默认不开启视觉。测试按钮只使用程序在内存生成的测试 PNG 向当前生效的模型发起请求（独立视觉模型或对话模型），可能产生费用，但不会使用摄像头。此外（2026-11-07 左侧栏「摄像头」页），受信任主窗口 `voice_device_request` 支持 `capture_photo`（仅已允许设备；签发 15s 一次性 128 位随机票据并按需补发 `capabilities.vision` 后调用设备 `self.camera.take_photo`；命中票据的上传直接存入内存 `CameraPreviewStore` 并返回 `200 no-store`，不调外部视觉模型，伪造/重放/无票据仍 `403`）、`camera_frame({after_seq?})`（返回 `{running,online,session_state,has_camera_tool,unchanged,pending,last_error,error_seq,frame_seq,frame}`；命中 `after_seq` 时返回 `unchanged:true` 省略 `frame` 大图字节；非允许设备立即清帧并返回空态）与 `clear_camera_frame`（撤销设备或停服时同步清除）。Android 命令仅返回不支持；真实固件、真实模型与实机延迟留 FINAL 验收。

### NV13 Claude Code / Codex 专用 AgentRun（2026-10-15 起可开启）

AgentRun 是独立 ApprovalKind，和原有 ExternalTool、激光动作分开。语音调用只有已授权设备及有效 voice session 可以准备任务；模型不能通过参数指定 executable、工作目录、shell、凭据、MCP 配置或者 confirmed。任务为最多 4000 字符的 task，全部经 stdin 传递，不由 cmd/PowerShell 解释。argv 由实现固定：Codex 为 `exec --sandbox workspace-write --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --cd <工作目录> --json --color never -`；Claude Code 为 `--print --restricted --permission-mode acceptEdits --permission-prompts none --tools Read,Glob,Grep,Edit,Write --strict-mcp-config --mcp-config {"mcpServers":{}} --setting-sources "" --no-session-persistence --output-format stream-json --verbose`（不开放 Bash 与网络工具）。CLI 版本不再锁死，但 `--version`/`--help` 必须能识别且列出全部安全参数，否则返回 `agent_capability_missing`；探测按可执行文件的大小与修改时间缓存，文件变了会重新探测。

审批摘要绑定 provider、工作目录、executable、CLI 版本、argv/配置版本和任务；批准时按当前设置重新计算，不一致拒绝。审批卡的 agent 节点只展示任务、provider、工作目录名、版本和费用提醒，不返回目录全路径、密钥或内部 request；批准时仍须勾选确认，Agent 的确认文案是“我看过这个任务，同意在本机工作目录里运行 Agent（可能产生模型费用）”，卡片标题为“运行本机 Agent（Claude Code / Codex）”，不是激光机的现场确认文案。ApprovalService.consume_agent_run 是专用事务，验证 voice permit、数据库设备 allowed、摘要、期限和批准状态后单次消费，连同 run 记录和审计原子写入；另一条路径不能把 AgentRun 当普通 ExternalTool 或激光动作执行。

门禁：`prepare` 要求 `ApiService::strict_mode()`（`.env` 的 `MOSS_FUSION_STRICT=1`），未开启返回 `agent_strict_mode_required`，工具列表里也不出现 `agent_codex` / `agent_claude_code`；Windows 与 macOS / Linux（2026-10-25 起）之外的平台没有验证过的进程树约束，返回 `agent_sandbox_unavailable`。批准后在后台线程运行，进程树在 Windows 上受 Job Object 约束、在 macOS / Linux 上受独立进程组约束（见 [安全入口](safety.md) §4.2），运行中可停止。

schema v8 的 voice_agent_runs 最近 50 条记录包含 run id、approval id、提供者、工作目录（桌面状态仅目录名）、摘要指纹、状态、最多 256 KiB 输出、时间和短错误码；启动后 running -> result_unknown，不能自动重新执行。

桌面 Tauri：`voice_agent_status()` 返回 `configured`（配置里的开关）、`enabled`（配置开启**且**严格模式开启）、`strict_mode`、`providers`（每个已填 CLI 现在能不能用及原因码）与 `runs`（最多 50 条，运行中的带 `stoppable`）；`voice_agent_save({input:{enabled,workspace,codex_executable,claude_executable}})` 保存本机已有路径并验证敏感目录和明确的 exe，**开启时必须有工作目录和至少一个 CLI**，否则 `agent_config_invalid`；`voice_agent_stop({runId})` 停止正在运行的任务（进程树一起结束），返回状态并带 `stopped`，任务已结束时为 false。命令只经 capabilities/voice-agent.json 授权 main 主窗口（remote 回环，命令内固定本进程后端地址）；Android 不支持。短错误码：`agent_disabled`、`agent_strict_mode_required`、`agent_path_denied`、`agent_capability_missing`、`agent_task_invalid`、`agent_run_failed`、`agent_timeout`、`agent_stopped`、`agent_permission_expired`、`agent_config_changed`、`agent_approval_consumed`、`agent_result_unknown`、`agent_unavailable`、`agent_thread_spawn_failed`（2026-10-17 新增：批准后任务线程起不来时当场把运行记录收口为 failed，不再永远停在运行中）。探测（`--version` / `--help`）的 stdout / stderr 并发读取且进程受 kill-on-close Job Object 约束。

**剩余风险（已告知用户）**：CLI 自身的沙箱在 Windows 上不能阻止读取工作目录以外的文件；工作目录必须选成可公开给模型看的目录，界面上已写明。不得声称已实现机密文件隔离。

### NV17 固件 OTA（内置语音 HTTP 路由 + 主窗口 Tauri 命令）

内置语音服务在 `POST /xiaozhi/ota/` 的回包里带 `firmware{version,url}`：只有设备已允许**且**存在用户确认过的有效推送、并且推送版本比设备上报版本新时才给出，否则 `firmware` 为空（`ServerConfig::firmware` 为 `None` 时也一律为空）。`url` 是 `http://<OTA 回包 Host 或本机地址>:<HTTP 端口>/xiaozhi/firmware/<token>.bin`。

`GET /xiaozhi/firmware/{token}.bin`：令牌必须是 32 位十六进制（其他形式、包含 `../` 等一律 404）；令牌对应一条未取消、未到期、设备尚未上报新版本的推送；请求方地址必须等于这台设备最近一次 OTA 请求的来源地址，设备当前必须是已允许状态；发送前重新读文件并核对 SHA-256，不一致不发。首次下载把 `pending` 改为 `downloading`。设备下载或写入中断后，再次请求 OTA 可取回同一推送和令牌。下载中可重试，服务端在读取后复核当前设备许可、来源、推送修订及镜像 SHA-256，只有首次下载改变状态。成功返回 `application/octet-stream`，其余情况一律 404（不区分原因，避免探测）。下载本身不代表刷写成功。

桌面 Tauri 命令只在 MOSS 主窗口可用（n67，2026-11-07 起独立安全窗口已取消）：capability `voice-private-main.json`（`windows:["main"]` + 回环 remote）授权，处理函数开头再用 `require_private_settings_window` 把来源固定为本进程后端上的主窗口；Android 返回仅桌面：

- `voice_firmware_status()`：返回 `enabled`、`artifacts`（已导入固件：sha256/version/project/size/created_at）、`devices`（已允许设备的型号与当前版本）、`records`（推送记录：delivery_id、device_id、firmware_version、download_status、device_report_status、expires_at、revision）与 `now`、`offer_hours`。
- `voice_firmware_import()`：弹本机文件框（只筛 `.bin`，不接受前端给的路径），校验 ESP32 应用镜像头与数字版本（≤16 MB），按 SHA-256 存到运行目录 `firmware/<sha256>.bin` 并登记；文件完全重复时只登记不重写。取消选择返回 `null`，校验失败返回 `firmware_invalid`。
- `voice_firmware_manage({input})`：只接受三种 action，字段集合必须完全匹配，否则 `firmware_invalid`。`offer{device_id,sha256,confirmed:true}` 要求设备已允许、固件已导入，且固件带 ELF 哈希（可按内容比对，允许同版本号重新编译与降级，n87）或版本比设备当前版本新，否则分别返回 `firmware_device_invalid` / `firmware_missing` / `firmware_not_newer`；成功时生成绑定设备的下载令牌，有效期 24 小时。`cancel{delivery_id}` 未开始下载的直接删除，已开始下载的保留记录并让令牌失效；已完成或已取消返回 `firmware_conflict`。`delete{sha256,confirmed:true}` 在还有有效推送时返回 `firmware_in_use`，否则删除记录与文件，返回最新的 status。
### n61 设备唤醒词（内置语音 HTTP 路由 + 主窗口 Tauri 命令）

`GET /xiaozhi/assets/{token}.bin`：令牌 32 位十六进制、对应内存里一份未过期（30 分钟）的暂存资源包，否则 404；成功返回 `application/octet-stream` 且带精确 `Content-Length`（固件据此判断分区放不放得下）。资源包由桌面经 `VoiceServer::install_wake_assets(device_id, bytes)` 暂存，同时向该设备发 `self.assets.set_download_url{url}` 与 `self.reboot`；设备当前会话未上报该 user-only 工具时直接报错、不发送。

桌面 Tauri 命令与 NV17 一样只在 MOSS 主窗口可用（capability `voice-private-main.json`，处理函数复核 `require_private_settings_window`），Android 返回仅桌面：

- n72 工具清单：`voice_save_settings` 接受 `mcp_full_tool_list`（布尔，默认 false；变更后重连接入点），`voice_status.mcp` 新增 `full_tool_list`，`mcp.tools[]` 新增 `in_compact`（是否在精简清单里）。接入点 `tools/list` 与内置语音默认只列精简清单（`tests/golden/mcp_tools_compact.json` 的 8 个激光工具 + 电脑控制类工具），工具名、执行与门禁不变。
- r41（2026-11-16）工具参数：两种清单的 `run_calibration_grid_tool` 都列出材料测试字段 `test_type`（`matrix` / `interval`）、`x_axis` / `y_axis`（`power` / `speed` / `interval` / `passes`）、`interval_min_mm` / `interval_max_mm`、`passes_min` / `passes_max`、`labels`（单一来源 `laser_mcp::calibration_test_props`，原样转发到 §3.7）；完整清单的 `laser_workflow_tool` 另列图片 `image_mode`（与 `TONE_MODES` 一致）与 `line_interval_scale`，精简清单不列——语音拿不到图片文件，说明里引导用户在工作台上传并选择转换方式与质量预设。两点对位的点动与读位置不做成工具，只在工作台用。
- n71 常连模式设置：`voice_status.settings` 新增 `builtin_mqtt_enabled`（默认 false）、`builtin_mqtt_port`（默认 1883）、`builtin_udp_port`（默认 8888），经 `voice_save_settings` 保存：开关须为布尔值，端口 1～65535，MQTT 端口不能为 8883、不能与 `builtin_ws_port` / `builtin_http_port` 相同，否则 `voice_settings_invalid`。`voice_builtin_status` 新增 `mqtt_port` / `udp_port`（实际监听的端口，未开或没开成为 `null`），`sessions[].transport` 为 `websocket` 或 `mqtt`。OTA 回复只对已允许且 `Client-Id` 与绑定记录一致的设备签发 `websocket.token`，并在常连模式开成时附加 `mqtt{endpoint,client_id,username,password,publish_topic,keepalive}`（用户名为设备编号，密码为设备 token）。
- `voice_wakeword_status()` → `{word:{text,command,threshold}, base:{files,size,chip_model,text_font,emoji_count,wake_word}|null, devices:[{device_id,name,supported,tools_loading,screen_pack_size,screen_pack,screen_pack_state,screen_pack_style,screen_pack_pending_with_pack,wake_target}], running}`；`devices` 只含在线且已允许的设备（对话模式与未配模型的回声模式均在连接后自动发现设备自带 MCP 工具与屏幕规格），`supported` 表示固件上报了 `self.assets.set_download_url`，`tools_loading` 表示设备已声明 MCP 但 `tools/list` 尚未返回（界面显示“正在读取设备能力…”，不误判为固件不支持）；n70 起 `screen_pack_size` 为彩屏上报的 `[宽,高]`（单色屏或没报分辨率为 `null`），`screen_pack` 表示这台已刷 `moss-screen-2` 全屏画风（来自设备偏好）。n77（2026-11-09）/ n84（2026-11-13）起以设备整包下载完及「版本前缀 + 画风 + 16 位十六进制内容指纹」（`moss-screen-2:<style>:<16hex>`）为准：`screen_pack_state` 为 `none` / `pending_download`（已推送、等设备下载）/ `download_expired`（推送超过 30 分钟仍没下载完）/ `installed`（版本前缀、画风与底包/字库/屏幕内容指纹完全一致）/ `style_outdated`（n79：装的是换画风之前的画风包，要重刷开机待命画面才换；`screen_pack_style` 为已装的画风）/ `assets_outdated`（n84：版本与画风一致，但未带指纹或底包、字库档位、屏幕画面资源已变，可一键更新画风字库）/ `font_outdated`（n81：装的是未带中文字库的旧 `moss-screen-1` 画风包，可一键重刷补齐中文字库）；`screen_pack_pending_with_pack` 表示等下载的这次是否带画风（不带画风的包下载完会清掉已装标记）；`wake_target` 为最近一次推送的唤醒词 `custom` / `default`，没推过为 `null`。资源包下载 `GET /xiaozhi/assets/{token}.bin` 改为 64 KB 分块下发、仍带准确 `Content-Length`。
- `voice_wakeword_import_base()`：弹本机文件框（只筛 `.bin`），校验资源包格式、校验和与 `index.json`（≤8 MB），存为运行目录 `wakeword/base-assets.bin`；n84 起若新底包内容指纹与旧底包不同，自动清掉各台 `screen_pack_auto_at` 节流以便开启自动保持的设备即时同步；取消返回 `null`，失败 `wakeword_invalid` 且保留旧文件。
- `voice_wakeword_manage({input})`：字段集合必须完全匹配。`save{text,command,threshold}`（显示文字 1～16 字、拼音 2～6 个 `mn6_cn` 词表音节、阈值 0.05～0.95）；`clear_base{confirmed:true}`（移除底包并在原先存在底包时清掉 `screen_pack_auto_at`）；`push{device_id,target:"custom"|"default",confirmed:true,screen_pack?:bool}` 要求设备已允许（`wakeword_device_invalid`）、服务运行中（`voice_builtin_stopped`）、设备在线且支持（`wakeword_device_unsupported`），返回状态加 `pushed{device_id,size,target,screen_pack}`；n70 / n77 / n84 `screen_pack:true` 时按设备分辨率与当前画风把全屏画风及兜底中文字库一起打进资源包（设备没报彩屏分辨率返回 `screen_pack_unsupported`），推送后把 `builtin_device_prefs.<设备>.screen_pack_pending` 记为 `moss-screen-2:<画风>:<16位内容指纹>`（设备整包下载完转入 `screen_pack`），不带画风推送时记为 `none` 并在下载完清掉已装标记。
- `voice_usb_provision({input})`（n92，2026-11-14；仅桌面主窗口可用，经 `capabilities/voice-private-main.json` 与 `require_private_settings_window` 校验，Android 返回 `voice_desktop_only`）：通过 `espflash` 串口工具链直读/直写 ESP32-S3 小智设备的 `0x9000` NVS v2 分区（读写产生的临时镜像存放于受 `0o700`/`0o600` 权限保护的 `usb-nvs-tmp` 目录，退场经 `TempFileGuard` 覆写清零删除，启动 `init` 同步扫除遗留临时文件）。串口相关只接受三种 `action`（n103 起另有两个只读 Wi-Fi 动作，见本条末尾；请求枚举及嵌套 `patch` / `new_wifi` 均严格 `deny_unknown_fields`）：`ports`（返回 `{ports:[{port_name,description,vid,pid,serial_number,is_usb,is_laser_port,is_likely_esp}],laser_busy}`，macOS 按 `/dev/cu.*` 去重同硬件 `tty.*`，并标注是否命中 `verified_connection()` 或 `GRBL_DEFAULT_PORT` 回落激光机串口）、`inspect{port}`（读分区表与 NVS 后触发芯片复位，拒绝加密 NVS、`nvs_keys` 与任何含 `localfs`/`fluidnc`/`spiffs`/`littlefs`/`ffat` 分区的激光控制器板卡，返回 `{port,device:{chip,mac_address,flash_size,flash_bytes,revision},partition:{label,offset,size,all_labels,has_xiaozhi_markers},nvs:{wifi_slots:[{slot,ssid,has_password,channel}],ota_url,max_tx_power,remember_bssid,sleep_mode,sleep_dim_min,sleep_off_min,board_uuid,has_ws_token,ws_url,namespaces},known_device}`，绝不回传 Wi-Fi 明文密码；n96 起 `sleep_mode` / `sleep_dim_min` / `sleep_off_min` 在设备上没有该键时为 `null`（固件按“开启 + 板型默认时间”处理），`wifi_slots` 只列 ssid 与 password 两键都在的槽位（与固件 `SsidManager` 一致）；`inspect` / `write` 均拦截激光作业/不确定作业及处于激活期且未撤销的固件下载 `downloading` 状态 `usb_blocked_firmware_busy`）、`write{port,expected_mac?,confirmed,auto_allow?,unrevoke?,patch?:{retain_slots?,new_wifi?:{ssid,password},ota_url?,max_tx_power?,remember_bssid?,sleep_mode?,sleep_dim_min?,sleep_off_min?,board_uuid?,ws_token?,ws_url?}}`（强制要求 `confirmed:true`、一致的 `expected_mac` 以及 `partition.has_xiaozhi_markers` 或 `board`/`websocket` 命名空间，未检出小智特征返回 `usb_wrong_device` 拒绝写入；拒绝占用激光机串口与激光/固件并发；仅当开启 `auto_allow` 且未清空 `ota_url` 与 `ws_url` 时按设备绑定或生成的 `board:uuid` 通过 `TokenSigner` 预签 `websocket:token` 并经 `bind_usb_voice_device` 在 `voice_devices` 预先登记/换绑 `client_id` 为 `allowed` 且保留原机型固件记录，关闭 `auto_allow`、清空 `ota_url` 或清空 `ws_url` 时同步抹除旧 `websocket:token` 与 `websocket:url`；写前内存往返校验后写入闪存并复位，返回 `{ok:true,port,device,partition,nvs,known_device,auto_allowed}`）。**n96（2026-11-14）修订**：`auto_allow` 缺省改为 `false`；设备在 `voice_devices` 中为 `revoked` 时，`auto_allow:true` 还须带 `unrevoke:true`，否则返回 409 `usb_device_revoked`；ESP32-P4（`device.chip == "esp32p4"`，上报的 Device-Id 是协处理器 MAC）不自动放行、不预签密钥，返回 `auto_allow_skipped` 说明。写入改为：芯片停在下载程序内写入 → 芯片侧 `FLASH_MD5` 与镜像比对 → 写入出错或不一致时把读出的原始 NVS 写回并再校验（成功返回 502 `usb_flash_write_rolled_back`，再失败返回 502 `usb_flash_restore_failed`）→ 复位；写入已校验无误而复位出错时仍算成功，返回 `reset_warning`。`patch` 中 `max_tx_power` / `remember_bssid` / `sleep_*` 不传即不动设备原值，`sleep_dim_min` / `sleep_off_min` 传 `-1` 擦除该键回到板型默认、`0..=1440` 为分钟数（仅在本次提交了等待时间时校验“关屏须晚于变暗”）；`ota_url` 按固件 `wifi_config::IsValidOtaUrl` 同一规则校验；`new_wifi.ssid` 按原样保存（只拒绝全空白）。分区表还须满足 NVS 起点按 4 KiB 对齐、不早于 `0x9000`、不与其他分区重叠、不超出芯片 Flash 容量；NVS 中键名不是合法 UTF-8 时拒绝读写（`usb_nvs_invalid`）。macOS 上 `/dev/tty.X` 与 `/dev/cu.X` 视为同一串口。说明：官方固件实际使用的协议由 OTA 应答决定且 OTA 应答会覆盖 `websocket` 设置，所以“开机直连 MOSS”真正起作用的是写入的 `ota_url` 加上 MOSS 里的“已允许”记录，预签的 `websocket:url` / `token` 只是兜底。
  **n103（2026-11-22）新增两个只读动作**（不碰串口、不占配网锁、不写诊断，请求同样 `deny_unknown_fields`）：`wifi_current{}`（电脑当前连接的 Wi-Fi）与 `wifi_scan{}`（扫描附近，Windows 监听 `WlanRegisterNotification` 扫描完成通知提前返回、最长兜底 3.5 秒，macOS 数秒），均返回 `{platform:"macos"|"windows"|"linux",permission:"granted"|"not_determined"|"denied"|"services_off",current:{ssid,signal,bands,secure,wpa3_only?,current}|null,networks:[同结构],hint:string|null}`；`signal` 为 0–100，`bands` 取 `2.4g` / `5g` / `6g`，`secure` 未知时为 `null`，`wpa3_only` 仅在可判定且网络只宣告 WPA3（不含 WPA2 过渡模式或开放，macOS 查 `CWSecurity`、Windows 解析 BSS 列表 RSN IE Tag 48 AKM、Linux 解析 `nmcli` `SECURITY`）时为 `true`、`false` 或省略；`wifi_current` 的 `networks` 恒为空数组；同名多 AP 合并（任一 AP 兼容 WPA2 / 开放即不算 `wpa3_only`），隐藏网络与超过 32 字节的名字不返回；从不返回任何 Wi-Fi 密码。没有定位授权（macOS / Windows 11 24H2 起）时不报错，`permission` 非 `granted` 且 `hint` 说明去哪里打开；电脑 Wi-Fi 开关关着（macOS `powerOn` 为假、Windows 所有射频软 / 硬开关关闭、Linux `nmcli radio wifi` 为 `disabled`）时返回 503 `wifi_radio_off`；没有无线网卡、WLAN 服务未运行或没有 `nmcli` 时返回 503 `wifi_unavailable`。另有 `open_wifi_settings{target:"location"|"wifi"}` 打开系统设置的定位权限页或 Wi-Fi 设置页，只用固定地址（macOS `x-apple.systempreferences:com.apple.preference.security?Privacy_LocationServices` / `x-apple.systempreferences:com.apple.wifi-settings-extension`，Windows `ms-settings:privacy-location` / `ms-settings:network-wifi`），不接受任意地址，成功返回 `{opened:true}`，Linux 返回 503 `wifi_unavailable`。macOS 上桌面命令在查询前于主线程调用 `moss_voice_host::prepare_wifi_location_permission` 请求定位授权。另 `lan_ipv4_addrs` / `ota_ip_url` 选址（2026-11-22）：排除代理 TUN、Tailscale 与虚拟机网卡后，优先取系统路由表默认出网的真实物理网卡；代理接管默认路由时有线网口（`以太网`/`Ethernet`/`eth*`/`enp*`）优先于无线网口，避免双网卡电脑默认向板子写入不稳定的动态 Wi-Fi IP。

### NV14 本地音乐（桌面 Tauri 命令，非公共 HTTP）

`voice.builtin_music` 为默认关闭的配置 `{enabled:false,directory:""}`；现有用户升级后不会自动扫描任何目录。所有命令仅由桌面主窗口的 `capabilities/voice-music.json` 授权（remote 回环，命令内固定本进程后端地址）；普通远程 Web 和 Android 没有真实实现。

- `voice_music_status()` → `{settings:{enabled,directory},running,requires_chat:true}`，纯读配置不扫描。
- `voice_music_pick_directory()` → `{directory:string}`，用户取消为 `null`；用户明确点击后弹出原生文件夹对话框，此时不保存也不扫描。
- `voice_music_save({input:{settings:{enabled:boolean,directory:string}}})` → 设置及 `restarted`、`track_count`；只有 enabled=true 且提供本机目录时建立只读索引；启用状态修改会重启当前内置语音服务、结束旧会话。

模型工具 `play_music({query?:string,random?:boolean})` 只能按歌名或随机二选一选择索引中的曲目。歌名限制为 1..200 字符；精确匹配优先，歧义返回最多5个候选，不自动播放。受控会话收到 `PlayMusic{track_id,title}` 后从受信索引按句柄/size/mtime 重查文件，只在当前有效许可下把 Symphonia 解码的24kHz单声道 PCM 交给已有60ms Opus泵。模型不能指定路径、目录、任意 URL、外部播放器或在同轮同时调用其他工具；歌曲不写入对话历史。切歌、唤醒、abort、自然插话、设备撤销或断线均取消当前源与旧 generation。假小智与合成 WAV 已通过软件测试；真实 MP3/FLAC/Ogg 文件及实体听音未验收。

### 其他桌面 Tauri 命令（非 HTTP 路由，2026-11-15 按 `generate_handler!` 补登）

以下命令已在 `apps/desktop/src-tauri/src/lib.rs` 注册，此前本文未列出；字段以处理函数为准。

- **NV15 家居（Home Assistant）**：`voice_home_status()`、`voice_home_save({input:{settings,expected_revision}})`（只接受这两个键，拒绝令牌与控制字段；版本冲突 409 `home_conflict`）、`voice_home_token_save({input:{action:"set"|"clear",expected_revision,value?}})`、`voice_home_voice_consent({input:{enabled,policy_revision,credential_revision}})`（语音只读授权）、`voice_home_control_consent({input:{enabled,policy_revision,credential_revision,confirmed}})`（语音控制授权）、`voice_home_discover()`、`voice_home_approvals()`、`voice_home_approval_approve({input:{approval_id,digest,confirmed}})`、`voice_home_approval_cancel({input:{approval_id,digest,confirmed:true}})`、`voice_home_backup_export()`、`voice_home_backup_audit()`（原生文件框选单个历史文件，只返回 `{risk,inspected_bytes,safe_to_share:false}`，取消为 `null`）、`voice_home_backup_restore({input:{profile,expected_revision,expected_credential_revision,confirmed:true}})`。字段集合须完全匹配，否则 400（`home_invalid` / `home_token_invalid` / `home_approval_invalid` / `home_backup_invalid`）。全部经 `capabilities/voice-private-main.json` 授权并在开头调用 `require_private_settings_window`；移动端返回仅桌面。门禁与审批语义见 [安全入口](safety.md) §4.4。
- **NV16 声纹**：`voice_speaker_manage({input})`，`action` 取 `status` / `cancel` / `begin{device_id,session_id,name,confirmed}` / `delete{id,revision,confirmed}` / `save_memory{id,revision,summary}` / `personal_mode{enabled,confirmed}` / `begin_test{device_id,session_id,id,confirmed}` / `calibrate{confirmed}` / `reset_calibration{confirmed}`，字段集合须完全匹配；授权同上。标定规则见 [安全入口](safety.md) §4.4 与 [声纹登记](../integration/speaker-enrollment.md)。
- **严格模式开关**：`voice_strict_set({enabled,confirmed})`（`default` capability，开头 `require_main_window`）。未确认 `strict_confirm_required`；后端未启动 `voice_unavailable`；写 `.env` 失败 `strict_write_failed`。成功返回 `{enabled, applied, needs_restart, bridge_restarted, env_file, bridge_error?}`，并重启小智 MCP 桥。
- **导出文件**（`default` capability，开头 `require_trusted_app_window`）：`get_export_settings()` / `pick_export_dir()` / `reset_export_dir()` / `set_export_ask_each_time({enabled})` 返回 `{dir, default_dir, effective_dir, ask_each_time, available, supported}`（`pick_export_dir` 取消为 `null`）；`save_export_file({filename,bytes,fallbackDefault?})` 返回实际保存路径，“每次询问”模式下取消为 `null`；`open_export_dir()`、`reveal_exported_file({path})` 在系统文件管理器中打开。界面行为见 [前端行为](frontend-behavior.md)。
- **窗口与后端地址**：`get_backend_origin()` 返回本进程后端来源；`minimize_to_tray()` 把受信任窗口隐藏到托盘 / 菜单栏。

## 1. 通用约定

### Rust 发送状态增补（2026-09-19）

- 完整 workflow/材料矩阵开始发送前，完整 `$$` 响应、`$32=1`、有限正数的 `$30` 和 Idle（含 `Pn:` 无 `E`/`F`/`D`、无 SD 运行标记）必须通过；`$30` 不是 1000 时按 `$30/1000` 在发送流水线中换算 S 值，原文件与 SHA 门禁不变。
  握手失败返回 400 `device_not_ready`，功率/状态检查失败返回 400 `device_preflight_failed`；不写加工指令。
- 活跃 `send_result` 增加 `phase`：`streaming` 或 `awaiting_completion`；兼容已有 `sending`/`running` 任务状态。2026-11-26（技术书 r68）起，若发送前实时读取发现固件未完整报告 X/Y 行程且软限位未开启或状态未知（I1），或在满足“回零已开启 + 轴行程已知 + 机械坐标区间方向确定 + 拿到起点 `MPos`”条件时按 `origin_mpos + motion_extents` 估算任务会撞到行程尽头（I2），初态与终态 `send_result` 均附带非阻断警告数组 `warnings: string[]`（只警告、绝不因此拒绝发送；无警告时不带该键）。
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
- 上限：`/api/upload` 20 MiB；`/api/draw/preview` 10 MiB；`/api/draw/state` 8 MiB；其余 64 KiB。
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
| gcode_preview_url | G-code 渲染的 PNG 预览 |
| gcode_download_url | `/preview?path=<gcode>&download=1` |
| image_preview_url | 预览图 URL（`/preview?path=...`） |
| scene_download_url | 仅 `/api/draw/preview`（及画板对位）：`.excalidraw.json` 场景 |

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
| `http://127.0.0.1:5173/5177/5178`、`http://localhost:5173/5177/5178` | 前端开发 / 预览端口；**仅 debug 构建下的回环请求**允许（release 构建拒绝，如需放行须显式配置 `MOSS_ALLOWED_ORIGINS`） |
| `MOSS_ALLOWED_ORIGINS` | 额外来源，逗号分隔，默认空；含 `*`、缺少 scheme 或带路径时宿主启动失败 |

| 请求 | 处理 |
|---|---|
| 不带 `Origin`（插件、MCP 桥接、curl、进程内调用、局域网配对客户端） | 行为不变；响应不带任何 `Access-Control-Allow-*` 头 |
| 带允许的 `Origin` | 正常路由；响应回显 `Access-Control-Allow-Origin: <origin>`、`Vary: Origin`、`Access-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS`、`Access-Control-Allow-Headers: Content-Type, Authorization, Accept, X-Moss-Desktop-Session`；`OPTIONS` 另带 `Access-Control-Max-Age: 86400` |
| 带不允许的 `Origin`，方法为 `GET` / `HEAD` | 正常路由但不带任何 CORS 头（跨站页面读不到响应） |
| 带不允许的 `Origin`，其他方法（含 `OPTIONS` 与 `text/plain` 简单请求） | 403 `{"success":false,"result":"请求来源不在允许范围内","detail":{"error_code":"origin_not_allowed"}}`，**不进入路由、不执行业务** |

**回环连接的 Host 校验（防 DNS 重绑定）**：对端为回环地址且请求带 `Host` 时，`Host` 的主机名必须是 IPv4 / 方括号 IPv6 字面量、`localhost`（不区分大小写）或 `MOSS_ALLOWED_ORIGINS` 中出现的主机名，否则**任何方法**（含 `GET`）都返回 403 `{"success":false,"result":"请求的主机名不在允许范围内，请用 127.0.0.1 或 localhost 访问","detail":{"error_code":"host_not_allowed"}}`，不进入路由。原因：攻击者把自己的域名解析到 127.0.0.1 后，浏览器认为页面与 MOSS 同源，`Origin` 校验和 CORS 都拦不住，但请求的 `Host` 仍是攻击者域名。非回环连接不做此项（仍由 §5 的 Bearer 鉴权保护），不带 `Host` 的非浏览器请求不受影响。

执行顺序：Host 校验 → 来源校验 → §5 鉴权 → 路由。

**并发连接名额与回环预留（2026-11-05）**：真实 TCP 入口同时最多 64 条连接。鉴权要等读完请求才能做，所以名额在接入时按对端地址分配：非回环连接最多占 56 条，最后 8 条只留给回环连接（桌面界面、Tauri 内嵌页走 127.0.0.1）。局域网慢连接或未鉴权连接占满后，本机界面的 `/api/safe-action` 急停等请求仍能接入；名额不足时在 200 ms 极短写超时内回 503 `{"success":false,"result":"并发连接过多，请稍后重试"}`，不读请求、不进路由、不拖住主 `accept` 循环。实现：`try_admit_http_connection`（`MAX_CONCURRENT_HTTP`、`LOOPBACK_RESERVED_HTTP`）。

**请求体长度头校验（2026-11-19）**：TCP 读取阶段与路由阶段共用 `parse_content_length` 校验；`Content-Length` 非纯数字、溢出或存在多个不同值时统一返回 400 `{"success":false,"result":"Content-Length 请求头无效或存在冲突","detail":{"error_code":"invalid_content_length"}}` 并关闭连接；携带 `Transfer-Encoding` 时统一返回 501 `{"success":false,"result":"不支持 Transfer-Encoding 分块传输，请使用 Content-Length","detail":{"error_code":"transfer_encoding_unsupported"}}` 并关闭连接。

## 2. GET 路由

| 路径 | 参数 | 成功 `result` | 失败 |
|---|---|---|---|
| `/api/health` | 无 | `{"status":"ok"}` | 无 |
| `/api/system/info` | 无 | `{version, auto_check_update, check_prerelease, process_trace_enabled, can_edit_operator_settings, can_write_reference_data, can_acknowledge_stop, actual_port, preferred_port, is_fallback, web_host, mcp:{configured, connected, endpoint(token 脱敏，未配置为空串), client_info, last_request, last_request_time, messages_count, tools_count, protocol_version, recent_events[], endpoints[], core_tool, safety_gate, estop_channel}, voice:{mode, device_sessions, phase, level}}`。`auto_check_update` 未在 `settings.json` 显式配置时默认为 `true`（2026-11-07），`check_prerelease` 默认为 `false`，`process_trace_enabled`（完整过程记录，n105）默认为 `false`、这里只给开关不给目录与占用；2026-11-26（r63）起新增三个按当前请求身份实时计算的只读布尔位 `can_edit_operator_settings`（能否修改端口/Jev/过程记录等操作员设置）、`can_write_reference_data`（能否写入材料库、雕刻路线与文字对刀起点）、`can_acknowledge_stop`（能否确认清除状态未知停机记录）。`configured` 只表示填了接入点地址；`connected` 表示桥当前与接入点保持连接；`client_info`、`last_request`、`recent_events` 无真实数据时为空，`messages_count` 为真实收到的帧数（无则 0），`protocol_version` 为最近一次 `initialize` 握手里 MOSS 实际应答的 MCP 协议版本、还没握手时为空串（2026-11-07 n68，原先写死 `2024-11-05`；控制台显示“未知（尚未握手）”），`core_tool`、`safety_gate`、`estop_channel` 是固定的设计说明、不是检测结果，`tools_count` 为当前开关下实际暴露的工具数（安全 19 / 开发者 20，再减去严格模式不开放的与用户关闭的工具）；`endpoints[]`（NV8）为每个接入点的 `{id, endpoint(脱敏), connected, connections, last_error}`，`id` 为 `primary` 或 `extra-N`，`connected` 为任一接入点已连接。`voice.mode` 为 `official` / `builtin`（取自运行目录 `settings.json`；2026-11-17 起自建链路删除，遗留的 `self_hosted` 按 `builtin` 报告）；`device_sessions` 在内置链路下来自内置语音服务（未运行为 `null`），官方链路为 `null`；`phase`（`idle` / `listening` / `thinking` / `speaking` / `null`）与 `level`（`0..=100` 归一化 RMS 振幅）仅在 `builtin` 链路有会话时由内置语音服务实时写入（不含对话原文或音频样本），其他链路为 `null` / `0` | 无；非回环也免鉴权 |
| `/api/system/lan-info` | 无 | `{lan_access_saved, lan_access_active, web_host, port, addresses:[本机局域网 IPv4], restart_required, static_pin_set, paired_devices_count}`；`restart_required` 为保存值与当前监听不一致 | 非回环 403 `仅允许在电脑本机查看` |
| `/api/ui/runtime-config` | 无 | `{connection:{default_mode, preferred_modes:[默认, 另一], serial:{default_port:"", baudrate:115200}, network:{default_host:"", transport:"telnet", http_port:80, telnet_port:23, timeout:5.0}}, upload:{gcode_suffixes:[".gcode",".nc"]}}` | 无 |
| `/api/ui/serial-ports` | 无 | `{ports:[…], default_port, baudrate}`；只列系统真实识别到的串口 | 无 |
| `/api/ui/discover-controllers`（**POST**，body `{}`；用 POST 防止跨站页面用无 Origin 的 GET 触发扫描） | 无 | `{controllers:[{ip, telnet_port, hostname, http_port, source: mdns/scan/mdns+scan, firmware, state, machine}], busy:[{ip, telnet_port, workflow_id}], checked, not_controllers, hint}`。2026-11-09 起：mDNS（`_telnet._tcp` / `_http._tcp`，约 1.5 秒）与本机局域网网卡所在 /24 的 23 端口扫描（枚举网卡、跳过回环与代理 / VPN 点对点虚拟网卡）只产生候选，逐台连接后发实时状态查询 `?` 与 `$I`（均只读，不发运动、出光或改设置的命令），回了状态报告、`[VER:…]` 或 GRBL 启动横幅的才列入 `controllers`（`machine` 为 FluidNC `$I` 的 `[MSG:Machine: …]`）；候选一出现就开始确认（mDNS 1 秒窗口内边找边验，回了别的协议立即判定、0.7 秒无回应即放弃），通常约 1 秒返回；正在加工或待确认停机的目标不另开连接，列入 `busy`；连不上或不是 GRBL 的只计入 `not_controllers`；并发调用由单飞锁防重入（已有扫描进行中时直接返回空列表与提示），扫描与验证线程均设有上限。没有结果时 `hint` 写明扫了哪个网段。候选来源可由 `ApiService::with_controller_candidates` 替换（测试不扫真实网络）。 |
| `/api/ui/material-options` | 无 | `{materials:[{name, thicknesses:[数字升序], surface?}]}` 按 name 排序；有表面通用条目的材料带 `surface:true`（表面条目不进厚度列表，只有表面条目的材料 `thicknesses` 为空） | 500 `读取材料选项失败` |
| `/api/ui/material-recommendation` | `material` 必填、`laser_mode` 必填、`thickness_mm`、`engraving_mode` 可选、`power_scale` 可选 | 白名单投影：`material, thickness_mm, matched_thickness_mm, match, laser_mode, laser_max_power, feed_rate, passes`；engrave 且有值时加 `engraving_mode`；r40 起条目随材料保存了图层设置时另带 `fill_options` / `cut_options`（没有就不带这两个键） | 400 `material 和 laser_mode 不能为空` / `读取材料推荐失败` |
| `/api/material-lab/export` | 无 | `{params_file, library:{version:1, power_scale, materials}}` | 500 |

### 2.1 `/preview` 文件服务

参数：`path` 必填；`download` 为 `1` 或 `true` 时附加 `Content-Disposition: attachment; filename="<文件名>"`。没有 `filename` 参数。

白名单，两条都要满足：

1. 后缀属于 `.png .jpg .jpeg .bmp .gif .webp .gcode .nc .json`。
2. 解析后位于运行目录的 `previews/`、`uploads/`、`generated_assets/` 之一（`ApiService::with_preview_roots`）。

成功：文件原始字节，`Content-Type` 按扩展名。失败一律 **404** JSON：`缺少 path` / `不支持预览该文件类型` / `文件不存在或不在允许的预览目录内` / `文件不存在`。

### 2.2 静态服务

`/`、`/draw`、`/draw/*`、`/material-lab`、`/twin`、`/voice`、`/camera`（`files::is_spa_route`）返回嵌入的 `frontend/dist/index.html`，`/assets/*`、`/vendor/*` 返回同目录静态资源（`rust-embed`，路径经 `safe_asset_path` 过滤）。`/material-lab` 是前端路由，首屏数据来自 §2.3。

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

成功 `result`：`{file_path, filename, size, suffix, preview_url}`，`file_path` 与 `filename` 都是保存后的文件名。保存名 `{unix 秒}_{sha1 前 10}_{stem 前 32}{suffix}` 到运行目录 `uploads/`；不登记数据库。HTTP 请求体与原始解码文件上限均为 20 MiB（前端对 >14 MiB 文件提前提示，避免 Base64 膨胀后触发请求体上限）。
失败 400：`不支持上传该文件类型`（detail `{filename, allowed}`）、`上传内容为空`、`上传内容不是合法 base64`、`上传文件为空`、`上传文件超出 20MB 限制`。不做图片魔数校验。

### 3.2 `/api/draw/preview`

| 字段 | 类型 | 必填 | 规则 |
|---|---|---|---|
| image_data_url | string | 是 | `data:image/…;base64,` 图片；解码并校验为图片，尺寸为 0 时 400 |
| scene_json | object 或 JSON 字符串 | 否 | 存为 `<image>.scene.json`；`mode=vector` 时据此直接生成矢量刀路（缺省时改用光栅并在 `vector_note` 说明）。**导入的 SVG**（2026-11-26，技术书 r66）：`type:"image"` 元素带 `customData.moss_vector` `{version:1, size:[w,h], paths:[{rings, open, fill, stroke}]}`（坐标为相对轮廓左上角的 mm；`rings` 为闭合环、同一项内按非零规则一起填，孔已按方向区分；`open` 为开放线）时按矢量处理、不算位图：按元素框等比拉伸，元素 `scale` 为负时翻转、`angle` 旋转；刻法 `canvas` 按 `fill` / `stroke` 填满 / 描边，`outline` 只描边，`fill` 填满并描边，`cut` 外轮廓与孔整组切（孔反向补偿）。版本不认识、缺尺寸、坐标非数字返回 400；超过 20000 个图形或 400000 个点拒绝。不带 `moss_vector` 的图片仍按位图（`has_image`）。导入 DXF 由前端转成同一格式（全部闭合轮廓放进同一项、按嵌套层数定方向，`fill:false, stroke:true`）。**摄像头参考底图**（r69）：`customData.moss_reference:true` 的图片元素永远跳过——不刻、不算位图、不计入排版范围，前端给激光的导出图也不含它 |
| material | string | 是 | |
| thickness_mm | float | 是 | >0 |
| mode | string | 否 | `vector`（2026-11-16 起前端默认）直接把场景元素转成刀路：手绘笔画按中心线一笔、不闭合，直线 / 箭头 / 矩形 / 菱形 / 椭圆按形状描边，有填充色的闭合图形与文字元素用矢量扫描线填满（文字元素转字形轮廓后先焊接，交叠笔画合成一笔，2026-11-17 技术书 r44；`laser_core::vector_plan::scan_fill`，设置见 `fill_options`），先填充、再描轮廓与笔画，2-opt 缩短空走；场景含用户插入（上传）到画板的图片时，`vector_style=cut` 返回 400 明确拒绝（r63，禁止带切割高功率慢速参数回退为整幅位图扫描），非切割刻法改用 `raster` 并返回 `vector_note`，结果另含 `path_optimization`、`fill_options`（有填充时为实际设置，否则 null）、`removed_duplicates`；`raster`（转图片光栅扫描：2026-11-17 技术书 r45 起与 §3.4 图片预览同一套引擎——透明按白色、按材料线距（可乘 `line_interval_scale` 0.5～3）重采样到成品尺寸，`image_mode` 等图片模式字段同图片预览（默认 `auto`，线稿多按 Otsu 阈值），不支持 `pass_through`；过扫描、双向与扫描偏移取 `fill_options`（同图片预览）；结果另含 `image_mode`、`pixel_size_mm`、`scan_options` 等，同图片预览。此前固定 0.1 mm 点距、灰度 128 一刀切、过扫描固定 1 mm）；`outline`（导出图沿深色区域描边，`trace_grayscale` + 路径优化 + `write_outline`）；其他值 400。未传时按全局开关（§3.10c：矢量 → `vector`，转图片 → `raster`）；结果另含 `engrave_route`（`vector` 或 `image`） |
| width_mm、height_mm | 可选正数 | 否 | 成品尺寸（2026-10-14 起生效，此前被忽略）：**原尺寸（2026-11-26，技术书 r70）**：`mode=vector`、宽高都缺省且场景（只刻选中时为整幅画）里有带 `moss_vector` 的导入图形时，按 `moss_vector.canvas_mm_per_unit`（缺省 0.25 mm / 画板单位）1:1 排版、不加四周 16 单位留边，成品 = 图形外框 + 过扫描 / 引线余量；超出 `work_area_*` 时整体等比缩小（余量不缩），且不拉满行程：每轴两侧各留该轴行程的 2%、至少 2 mm（r71，宽高留空按 0.1 mm/像素超出时的自动缩小同样留这份余量），结果 `auto_fitted:true`；两种情况结果都带 `native_size:true` 与说明 `fit_note`。`canvas_mm_per_unit` 须为 `(0, 100]` 的有限数字，否则 400。其余情况如下：都填时锁定比例放进该框；只填一边按比例算另一边；缺省、空串、null 或 0 按导出图每像素 0.1 mm。2026-11-06 起按机器幅面（默认 100 × 100 mm，可用 `work_area_width_mm` / `work_area_height_mm` 覆盖；光栅模式宽度另留两侧过扫描，r45 起按实际过扫描）而不是 400 mm 画布：填写的尺寸超出返回 400；没填且按 0.1 mm/像素超出时等比缩小并返回 `auto_fitted=true` 与 `fit_note` |
| work_area_width_mm、work_area_height_mm | 可选正数 | 否 | 2026-11-26（技术书 r64 / r68）：机器有效幅面（mm）。缺省或 `null` 时优先采用最近一次工作台握手持久化在 `laser_verified_connection.machine_limits` 中的固件 X/Y 行程（后端只知固件值、不知界面手填值；界面请求总会显式传入 `work_area_*`，故界面“手填值优先”不受影响），无记录时回退默认 100 × 100 mm（翼宿 V1.0 默认幅面）；显式提供或取自记忆记录时均须为 `(0, 2000]` 内的有限数字（对齐前端 `MAX_XY_TRAVEL_MM = 2000`），非数字、非有限数、`≤ 0` 或 `> 2000` 直接返回 400，不静默回退；前端仅在当前连接目标与行程保存时的 `target_key` 一致时透传该组幅面 |
| vector_style | string | 否 | 仅 `vector`（2026-11-17 技术书 r46 / r47）：`canvas`（按画布颜色：设了背景色的闭合图形填满、线条颜色可见的描边，文字填满并描边；缺省值，只为接口兼容）/ `outline`（闭合图形与文字只描边，只设了背景色的图形也描边）/ `fill`（闭合图形与文字都填满并描边）/ `cut`（r47：沿闭合图形与文字轮廓切穿，开放线条沿线切；先内后外规划，切缝补偿按嵌套层数定方向——偶数层是零件外框往外、奇数层是孔往孔里，同一段文字整组补偿；规划后再加桥位、引线、过切；必须有该厚度的切割参数，否则 400“材料库里还没有「…」的切割参数…”）；手绘、开放直线与箭头在非切割刻法下始终刻一笔。`fill` 取材料库填充（raster）参数，`cut` 取切割参数，其余取描边参数。结果回显 `vector_style`；其他值 400。前端画板提供 `outline`（默认）/ `fill` / `cut` 并总是显式发送 |
| vector_simplify_factor | 可选数字 | 否 | 仅 `outline`：0.25 ~ 8，默认 3 |
| fill_options | object | 否 | 仅 `vector`，用于有填充色的图形与文字元素（线距取材料像素间距）；有填充时成品四周再让出过扫描距离，关光滑行也不越出成品范围。2026-11-16 对齐 LightBurn Fill 模式，全部可省略：`grouping`（`all` 默认，每行横穿所有图形、图形之间 `G1 S0` 匀速滑过 / `individual` 一块块蛇形扫完）、`angle_deg`（扫描角度，按机器视角逆时针，0 横扫，±360 内）、`cross_hatch`（再转 90° 扫一遍，默认 false）、`bidirectional`（默认 true；false 时回程不出光）、`overscan_mm`（每行两头关光多走的距离；省略、null 或 `"auto"` 按 v²/(2a) 自动、取 0.1 mm 整、限 1 ~ 20；数字 0 ~ 20，0 关闭）、`acceleration_mm_s2`（自动过扫描用，默认 1000）、`scan_offset_mm`（双向扫描错位补偿 ±2，每行逆着行进方向挪一半）、`scan_offset_table`（2026-11-16 对齐 LightBurn Scanning Offset Adjustment：`[[速度 mm/min, 偏移 mm], …]`，1 ~ 32 行，速度 > 0 且不重复、偏移 ±2，可乱序（按速度排好），按本次加工速度线性插值、低于首行 / 高于末行取端点值，给了就覆盖 `scan_offset_mm`；省略、null 或空数组不用表）；越界或类型不对 400 |
| selected_element_ids | string[] | 否 | 仅 `vector`（2026-11-16，对齐 LightBurn“只加工选中图形”，不用选区原点）：只刻这些 Excalidraw 元素（及绑在选中容器上的文字），排版（缩放、居中、过扫描留边）仍按整幅画算，选中部分刻在它在整幅画里的位置；1 ~ 10000 个非空字符串。非矢量模式、选中图片、选中部分没有可刻内容时 400；成功时结果含 `selected_only=true` 与 `selection_note` |
| machine_motion | object | 否 | 2026-11-16 按加速度估算工时：键名与 §3.3 `machine_limits` 相同——`x_accel_mm_s2`、`y_accel_mm_s2`（0 ~ 100000，只给一个时两轴同值）、`x_max_rate_mm_per_min`、`y_max_rate_mm_per_min`（G0 按两轴较大者走，再按方向逐轴封顶）、`junction_deviation_mm`（默认 0.01）；全部可省略，缺省按 1000 mm/s²。自动过扫描缺省时也取两轴加速度较小者。非数字或越界 400 |
| path_options | object | 否 | 仅 `vector`。2026-11-16 对齐 LightBurn 优化设置：`prefer_corners`（闭合轮廓起刀点优先放在转角 > 45° 的尖角，默认 true）、`remove_overlaps`（整条重复的轮廓 / 线段只刻一遍，容差 0.01 mm，默认 true）；切割先内后外由刻法决定，不能在这里关 |
| cut_options | object | 否 | 2026-11-17 技术书 r47 起只用于 `vector_style=cut`（矢量切割），键与 §3.4 文字外框切割相同（`kerf_mm`、桥位、引线、过切，默认全关），请求没写的键取材料库该厚度切割条目随材料保存的值；其他模式带非 null 的 `cut_options` 时 400“cut_options（切割设置）只用于画板矢量刻法“切割”或工作台文字的外框切割”。结果另含 `laser_mode`（`cut` / `engrave`）与 `cut_options`（同 §3.4 回显，含 `report`） |
| 连接字段 | 见 §1.5 | 否 | 只记录，不连接设备 |

后端直接生成光栅或轮廓 G-code（`generate_draw_artifacts`），写入运行目录 `generated_assets/`，创建 `source_type="draw"` 的 workflow 并冻结 `confirmation_snapshot`。
成功 `result` 含 `workflow_id`、`status=preview_ready`、`gcode_file`、`gcode_sha256`、`image_file`、`time_estimate`、`origin="lower_left"`、`origin_note`、`frame_mm:{min_x,max_x,min_y,max_y}`（供红光走边框）、`frame_hull_mm`（出光点凸包 `[[x,y],…]`，以激光头起点为原点，供橡皮筋走边框；含圆弧或出光点不足时为 null）、`artifacts.width_mm/height_mm/mode`（实际成品尺寸与生成方式）、`draw_lab:{job_summary, content_fingerprint}` 与 §1.4 的 `web`。`outline` 没有可描边的深色图形时 400。
2026-11-17（技术书 r49）起画板记录的 `draw_lab.job_summary` 按实际模式填写：`mode` 为 `vector` / `raster` / `outline`（此前矢量也写成 `raster`，摘要“策略”显示错），另有 `vector_style`（矢量刻法）、`task_type`（`cut` / `engrave`）与 `pixel_size_mm`（光栅线距）；2026-11-26（技术书 r68）起当有效进给速度大于 `machine_motion` 两轴最大速度中的较小值时，`artifacts.warnings` 与 `draw_lab.job_summary.warnings` 返回非阻断提示 `["雕刻速度 X mm/min 超过固件最大速度 Y mm/min，固件会自动降速，工时与出光效果会偏离设定"]`（不夹紧或修改原速度；未超速时不带该键）。
失败 400：各校验文案；`保存画板素材失败: …`。前端未收录的字段（`threshold`、`dither_algorithm`、`raster_quality_strategy` 等）当前被忽略。

### 3.2a `/api/draw/state`（创作画板持久化状态）

- **本机权限边界**：上游仍先执行来源与 Bearer 鉴权（非回环未鉴权返回 401）。进入该路由后，`GET` 与 `POST` 仅允许本机调用方（`CallerContext::Desktop` 与 `CallerContext::Loopback`）；局域网配对连接（`CallerContext::Lan`，其中 `POST` 由上游 `is_reference_data_write` 返回 403 `lan_read_only`，`GET` 由本路由返回 403）以及语音、MCP 桥、审批、网关调用方全部返回 `403`（`画板状态只能在本机界面访问`），避免局域网终端读出或改写桌面本机画布。
- **接口行为**：读写运行目录 `draw_state.json`，供桌面端在端口变化或重启后恢复画板（含 `exists`、`scene`、`last_preview`、`form`、`last_accepted_fingerprint`）。`POST` 请求体上限 8 MiB，支持局部字段补丁更新，已知字段的值格式不符返回 400。GET 或 POST 发现损坏的存档会先改名为 `draw_state.json.corrupt-<时间戳>`，再从空状态继续。
- **设备连接选择**：`form` 只保存 `material`、`thickness_mm`、`width_mm`、`height_mm`、`image_strategy`、`vector_style`、`image_mode`、`line_interval_scale` 和 `vector_simplify_factor`。其他合法键名（包括连接和凭据字段）直接丢弃。串口与网络目标仍由当前浏览器的画板设置或工作台继承逻辑决定，前端恢复时忽略旧存档中的连接字段。
- **非本机前端退路**：非本机浏览器（如局域网手机 / 平板端）收到 `GET` 的 403（或其他失败响应）时，保留前端现有的错误退回本机缓存机制——画布、图片与上个任务仅保存在各自浏览器的 `localStorage` 中，不读取桌面本机画布。

IPv6 网络目标显示为 `[<device-ip>]:<端口>`。续雕比对同一地址和端口，也接受早期记录的同目标无括号格式。

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

**总是 `success:true`**。`result`：`{preferred_mode, selected_mode|null, fallback_used, attempts:[{mode, target, connected, summary, laser_mode_ok}], serial_connected, network_connected, connected, firmware_settings, soft_limits_enabled:bool|null, homing_enabled:bool|null, identity:{firmware:string|null, version:string|null, machine_name:string|null}|null, machine_limits:{x_travel_mm, y_travel_mm, z_travel_mm, x_max_rate_mm_per_min, y_max_rate_mm_per_min, x_accel_mm_s2, y_accel_mm_s2, junction_deviation_mm, soft_limits_enabled, homing_enabled, homing_dir_mask}|null, features:{axes, speed_map, has_air_assist, has_door, has_estop_or_fault_pin, x_soft_limits, y_soft_limits, x_homing, y_homing, z_homing}|null, timeout_seconds, summary}`（其中 `x_homing` / `y_homing` / `z_homing` 为 `{cycle:int|null, mpos_mm:float|null, positive_direction:bool|null}|null`）。`machine_limits` 来自 FluidNC `$CD` 的 `max_travel_mm` / `max_rate_mm_per_min` / `acceleration_mm_per_sec2` / `junction_deviation_mm` / `soft_limits` / `homing:`（只认 x/y/z/a/b/c 轴段，`homing:`、`motor0:` 子段不改当前轴，同时把实际配置的轴段记入 `features.axes`，如 `["x","y","z"]`），或经典 GRBL 的 `$130/$131/$132`、`$110/$111`、`$120/$121`、`$11`、`$20`（软限位）、`$22`（回零）、`$23`（回零方向掩码）；`identity` 来自握手阶段额外发送的只读 `$I`（解析 `[VER:…]` 与 FluidNC `[MSG:Machine: …]`，未读到时为 `null`）。前端按连接目标 `target_key` 分桶保存机器行程、身份、运动参数与固件能力，行程按“手填 → 固件 → 默认 100 mm”取值；同一 `target_key` 再次握手若前后 `identity` 均非空且不一致，前端重置该目标的固件行程、手填行程与运动参数并提示换机（r68）。只有参数类型错或模式非法才 400。检查会真实握手（`$$` 到 `ok`、要求 `$32=1`），不发送运动或出光指令。n73 / r68：桌面、本机网页、局域网配对调用且握手成功时，后端记住这个连接及当次握手的固件 X/Y 行程（`settings.json` 的 `laser_verified_connection`，含 `fields` 与 `machine_limits:{x_travel_mm,y_travel_mm}|null`），接入点与内置语音调用激光工具没给连接参数或 `work_area_*` 时用它；语音 / 接入点身份做的检查不记。

### 3.4 `/api/text-task/generate`

| 字段 | 类型 | 必填 | 默认 |
|---|---|---|---|
| text、material | string | 是 | `text 不能为空`；2026-11-17（技术书 r44）起给了 `shapes` 或 `border` 时 `text` 可以为空（只刻图形，不读字体） |
| thickness_mm | float | 是 | >0，否则 400 `thickness_mm 必须是大于 0 的数字` |
| engraving_mode | string | 否 | 只用于判断是否切割（`cut`）；不再决定文字刻法（此前写 `raster` 也只描轮廓） |
| text_fill | string | 否 | `outline`（默认，2026-11-06 用户决定）只描字的轮廓，取材料库 outline 参数；`fill` 实心填充：2026-11-16 起改为矢量扫描线（线距 `pixel_size_mm`，从轮廓精确求交，不经过位图；扫描设置见 `fill_options`，默认一次填完、双向、过扫描自动；最后描一遍轮廓），取 raster 参数；`hatch` 排线填充，线距为 `pixel_size_mm` 的 3 倍并描边，取 outline 参数；切割（`laser_mode` / `engraving_mode` 为 `cut`）时闭合轮廓先内后外。填充按非零环绕规则，字内孔不填 |
| text_align | string | 否 | `center`（默认）/ `left` / `right`；文字里的换行即分行 |
| origin | string | 否 | 2026-11-17 技术书 r48 起缺省时按全局对刀起点（§3.10c `/api/text-origin`，未设置为 `center`）；`center`（用户决定的默认）开刻前激光头对准木牌（图案）中心；`lower_left` 对准木牌左下角（无木牌为文字左下角）。只改开头 `G92` 声明的激光头位置，结束时关光回到起点 |
| width_mm / height_mm（别名 board_width_mm / board_height_mm） | float | 否 | 木牌尺寸，可只给一边；没给 `font_size_mm` 时自动取能放下的最大字号并居中，给了字号但放不下 400 `…放不进 … 的木牌` |
| safe_margin_mm（别名 margin_mm） | float | 否 | 2；放进木牌时四周留边 |
| pixel_size_mm | float | 否 | 材料库对应参数（0.05～1）；`fill` 的扫描行距与 `hatch` 的排线间距 |
| fill_options | object | 否 | 只对 `fill` / `hatch` 有效。过扫描让扫描线越出笔画：放进木牌时两侧留边至少为过扫描距离；`lower_left` 且没给木牌时激光头在过扫描范围的左下角（`frame_mm.min_x` = 过扫描距离），运动不往起点负方向越界。2026-11-16 对齐 LightBurn Fill 模式，全部可省略：`grouping`（`all` 默认，每行横穿所有图形、图形之间 `G1 S0` 匀速滑过 / `individual` 一块块蛇形扫完）、`angle_deg`（扫描角度，按机器视角逆时针，0 横扫，±360 内）、`cross_hatch`（再转 90° 扫一遍，默认 false）、`bidirectional`（默认 true；false 时回程不出光）、`overscan_mm`（每行两头关光多走的距离；省略、null 或 `"auto"` 按 v²/(2a) 自动、取 0.1 mm 整、限 1 ~ 20；数字 0 ~ 20，0 关闭）、`acceleration_mm_s2`（自动过扫描用，默认 1000）、`scan_offset_mm`（双向扫描错位补偿 ±2，每行逆着行进方向挪一半）、`scan_offset_table`（2026-11-16 对齐 LightBurn Scanning Offset Adjustment：`[[速度 mm/min, 偏移 mm], …]`，1 ~ 32 行，速度 > 0 且不重复、偏移 ±2，可乱序（按速度排好），按本次加工速度线性插值、低于首行 / 高于末行取端点值，给了就覆盖 `scan_offset_mm`；省略、null 或空数组不用表）；越界或类型不对 400 |
| path_options | object | 否 | 2026-11-16 对齐 LightBurn 优化设置：`prefer_corners`（闭合轮廓起刀点优先放在转角 > 45° 的尖角，默认 true）、`remove_overlaps`（整条重复的轮廓 / 线段只刻一遍，容差 0.01 mm，默认 true）；切割先内后外由刻法决定，不能在这里关 |
| cut_options | object | 否 | 外框切割质量（2026-11-16 对齐 LightBurn Line 模式 / Cut 设置，融合技术书 r36），只用于切割（`laser_mode` / `engraving_mode` 为 `cut`），全部可省略、默认全关（不传与传 `{}` 的 G-code 完全相同）；雕刻任务带了生效的设置 400。`kerf_mm`（切缝补偿 ±2 mm，正值往外、负值往里，内孔自动反向，零件保持设计尺寸；按非零环绕先把重叠笔画合并，内缩时消失的小图形丢掉、全部消失 400，轮廓数变化写进回显 `note`）、`tab_count`（每个闭合图形的桥位个数 0 ~ 100 的整数）或 `tab_spacing_mm`（每隔多少 mm 一个，1 ~ 1000，至少 1 个；与 `tab_count` 二选一，都填 400）、`tab_width_mm`（默认 0.5，0.05 ~ 10；桥位沿周长均匀分布、第一个离起刀点半个间距，总宽不超过周长一半，放不下的图形不加）、`tabs_skip_inner`（默认 true，被别的图形包着的内部图形不加桥位）、`lead_in_mm` / `lead_out_mm`（0 ~ 10，总在废料一侧：外轮廓在外、内孔在孔里；会碰到任何轮廓或落到零件一侧的那一条不加）、`lead_angle_deg`（1 ~ 90，默认 90 = 沿起刀点两边法线的角平分线；越小越贴着起刀边从后方切入）、`overcut_mm`（0 ~ 10，回到起点后沿轮廓多切，最多一圈，落进桥位时停在桥位前）。桥位是 `G1 … S0` 关光匀速滑过；切缝外扩与引线计入木牌留边（同过扫描），`lower_left` 起点不往负方向越界。规划阶段的出光长度自检照旧（对补偿后的轮廓），桥位 / 引线 / 过切后处理另行自检（轮廓上走过的长度 = 周长 + 过切，出光总长 = 规划出光 − 桥位 + 引线 + 过切），不符拒绝生成；越界或类型不对 400（中文说明） |
| font_size_mm | float | 否 | 10.0；给了木牌尺寸且没填时自动适配 |
| letter_spacing_mm | float | 否 | 0.5 |
| font_path | string | 否 | 缺省使用内置字体 |
| work_area_width_mm / work_area_height_mm | float | 否 | 2026-11-26（技术书 r65 / r68）：缺省或 `null` 时优先采用最近一次工作台握手记在 `laser_verified_connection.machine_limits` 的固件 X/Y 行程，无记录时回退默认 100（翼宿 V1.0 幅面）；提供或取自记忆记录时均须为 `(0, 2000]` 内的有限数字，非数字、非有限数、`≤ 0` 或 `> 2000` 返回 400；成品超出时 400 `成品约 W × H mm，超出机器幅面…`（2026-11-06；r44 起含图形）。`/api/workflow` 图片预览同样按该规则取值并按 `(0, 2000]` 校验 |
| engrave_route | string | 否 | 2026-11-17 技术书 r45：`vector` / `image`，缺省按全局开关（§3.10c）。`image` 转图片：文字（一律实心，`text_fill` 的描边 / 排线不生效、结果 `text_fill=fill`）、填满的图形、描边图形与直线（线宽取 0.3 mm 与两倍线距的较大者）按材料线距（`pixel_size_mm`）渲染成黑白位图，网格与版面左下角对齐，再逐行扫描（`write_raster`，过扫描 / 双向 / 扫描偏移取 `fill_options`，偏移的一半不能超过过扫描），整件取 raster 参数；排版、起点、`frame_mm` 与矢量完全相同。外框切割（`laser_mode=cut`）始终走矢量并在 `route_note` 说明。结果另含 `engrave_route`（实际路线）、`route_note`（文字刻法被改为实心或切割改走矢量时的说明，否则 null）、`raster`（`{grid_width, grid_height, pixel_size_mm}`，矢量时 null）；位图超过 1600 万像素 400 |
| shapes | array | 否 | 2026-11-17 技术书 r44 基础图形（`laser_core::shapes`，轮廓按公式生成，曲线弦高误差 0.01 mm），最多 32 个（含边框线）。坐标 mm、**木牌中心为原点、x 向右、y 向上**。每项：`kind`（`circle` / `ellipse` / `rect` / `polygon` / `star` / `heart` / `line`），`x_mm` / `y_mm`（中心，默认 0），尺寸 `size_mm`（圆的直径、正多边形 / 星形的外接直径；`heart` 只给一边时宽:高 = 1:0.9）或 `width_mm` + `height_mm`（椭圆、矩形必须两边都有），直线用 `length_mm`（或 `width_mm`）；`rect` 可带 `corner_radius_mm`（超过短边一半按一半）；`polygon` 的 `sides` 3～24（默认 6）、`star` 的 `points` 3～24（默认 5）与 `inner_ratio` 0.1～0.95（默认正星形比例，五角星 0.382）；`rotation_deg` 逆时针；`fill`：`outline`（默认）/ `fill`（也接受布尔），直线不能填满。尺寸 0.5～2000 mm。正多边形、星形第一个角朝上。图形超出木牌（填满时含过扫描）400 `…超出了木牌…`；与文字墨迹框（外扩 0.2 mm）相交、或填满的图形把文字包在里面 400 `…和文字重叠了…`（只描边的外框围着文字不算）。有填满的图形时整件取材料库 raster 参数（同 `text_fill=fill`），填满一律按 `pixel_size_mm` 实心线距，扫描设置同 `fill_options`；切割（`laser_mode=cut`）时图形不能填满（400 `切割时图形不能填满`），切缝补偿对每个图形单独做 |
| border | object | 否 | r44 沿木牌边缘的边框（只描边）：`shape` `rect`（默认）/ `rounded` / `circle` / `ellipse`，`style` `single`（默认）/ `double`，`inset_mm`（外圈离木牌边缘，默认 3）、`gap_mm`（双线间距，默认 1.5）、`corner_radius_mm`（圆角，默认外圈短边的 12%）。矩形类要同时给木牌宽高，圆形给一边即可（取短边）。文字自动适配进最里圈再留 `safe_margin_mm` 的区域（圆 / 椭圆边框按椭圆区域适配，圆角边框避开圆角）；放不下 400 `…放不进边框里面…` |
| text_box | object | 否 | r44 文字区域 `{x_mm, y_mm, width_mm, height_mm}`（坐标同 `shapes`，必须在木牌内，否则 400 `text_box 超出了木牌范围`）：文字以它为中心自动适配（给图形留位置时用），优先于边框 / 整块木牌 |
| text_weld | bool | 否 | r44 字形焊接，默认 `true`：规划前把字形轮廓按非零环绕取并集（对齐 LightBurn “Weld”），笔画交叠或字与字相连的字体不在交叠处多刻 / 多切一道线；填满区域本来就按非零环绕，焊接前后相同。`false` 关闭 |
| machine_motion | object | 否 | 2026-11-16 按加速度估算工时：键名与 §3.3 `machine_limits` 相同——`x_accel_mm_s2`、`y_accel_mm_s2`（0 ~ 100000，只给一个时两轴同值）、`x_max_rate_mm_per_min`、`y_max_rate_mm_per_min`（G0 按两轴较大者走，再按方向逐轴封顶）、`junction_deviation_mm`（默认 0.01）；全部可省略，缺省按 1000 mm/s²。自动过扫描缺省时也取两轴加速度较小者。非数字或越界 400 |
| manual_params | object | 否 | n73：这一次临时指定的 `{power_percent 1~100, feed_rate 100~6000 整数 mm/min, passes 1~5 整数}`，各项可缺省；覆盖材料库推荐或已采用的调参建议，不改材料库；有未知字段或越界 400。图片预览（`/api/workflow` preview）与画板预览同样接受 |
| 连接字段 | 见 §1.5 | 否 | 只记录 |

后端用材料库推荐参数（或 `manual_params`）生成 G-code 与预览 PNG（运行目录 `generated_assets/`），创建 `source_type="text"` 的 workflow 并冻结快照。
r44 起成功 `result.artifacts` 另含 `shapes`（实际使用的图形，尺寸与默认值已补齐，`fill` 为 `fill` / `outline`）、`shape_count`（含边框线）、`border`、`text_box`、`weld`（`{contours_before, contours_after}`，没有文字或关闭焊接时为 null）；有图形时 `width_mm` / `height_mm` 与 `frame_mm` 是文字与图形合起来的范围，`origin=center` 时激光头仍对准**木牌中心**（不是版面中心）。r68 起当有效进给速度超过 `machine_motion` 两轴最大速度中的较小值时，`result.artifacts`（以及 §3.6 图片预览的 `result.artifacts`、§3.7 标定矩阵与测试图案的 `result.warnings` / `result.artifacts.warnings`）另带 `warnings: ["雕刻速度 X mm/min 超过固件最大速度 Y mm/min，固件会自动降速，工时与出光效果会偏离设定"]`，不夹紧或修改原速度。
成功 `result` 含 `workflow_id`、`status=preview_ready`、`gcode_file`、`gcode_sha256`、`image_file`、`font_path`、`material`、`thickness_mm`、`width_mm`、`height_mm`（笔画实际范围）、`font_size_mm`（实际字号）、`text_fill`、`origin`、`origin_note`（开刻前激光头放哪）、`board_width_mm`、`board_height_mm`、`frame_mm:{min_x,max_x,min_y,max_y}`（笔画相对激光头起点的范围，不含过扫描，供走边框）、`glyphs`、`path_optimization`、`fill_options`（实际使用的 `{spacing_mm, grouping, angle_deg, cross_hatch, bidirectional, overscan_mm, overscan_auto, scan_offset_mm, scan_offset_source:"fixed"|"table", scan_offset_table}`，`scan_offset_mm` 为实际生效值（用表时为插值结果），只描轮廓时为 null）、`cut_options`（切割时回显 `{active, kerf_mm, tab_mode:"off"|"count"|"spacing", tab_count, tab_spacing_mm, tab_width_mm, tabs_skip_inner, lead_in_mm, lead_out_mm, lead_angle_deg, overcut_mm, report:{contours, kerf_contours_before, kerf_contours_after, tabs, tab_gap_mm, contours_without_tabs, lead_ins, lead_outs, leads_skipped, overcut_mm}, note}`，非切割为 null）、`removed_duplicates`、`frame_hull_mm`（出光点凸包，同 §3.2）、`time_estimate:{estimated_seconds, estimated_minutes, laser_seconds, non_laser_seconds, model:"acceleration", acceleration_mm_s2:{x,y}, acceleration_source:"machine"|"default"}`（2026-11-16 起按 GRBL 规划器模型估算：梯形加减速、相邻两段按转角偏差公式限速、M3/M4/M5 切换与 G4 处停住；图片、画板、导入 G-code 与标定网格的 `time_estimate` 同样如此，都接受 `machine_motion`）与 §1.4 的 `web`。
材料安全门：后端调用 `laser_core::safety::evaluate_material`；PVC、聚氯乙烯及明确危险材料返回 400 `material_safety_blocked`，仅写“塑料”等成分不明描述时同样返回 400 并要求澄清。MCP 工具 `generate_text_laser_task_tool` 与 `laser_workflow_tool preview`（带 `text`，r44 起带 `shapes` 或 `border` 也算）都转发到本路由；工具的 `font_size` 按 `32px≈1mm` 折算为 `font_size_mm` 并限制在 3～80。

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
| `preview` / `plan` / `create` | （注：HTTP 直接传 `workflow_id` 撞上在途或已有记录时返回 409 `device_busy`/`workflow_id_conflict`。MCP 的 `laser_workflow_tool(action=preview)` 会丢弃传入的旧编号并生成新记录；含刀路产物时由服务端冻结真实 64 位 `gcode_sha256`）新记录含 `origin`（2026-11-06：发起方 `CallerContext::label`——`desktop` / `voice` / `mcp_bridge` / `gateway` / `lan` / `loopback` 等，服务端填写，请求自带的 `_origin` 被丢弃；采用反馈建议生成的新预览同样记录）。`source_type=text` 时等价于 §3.4；`source_type=image` 需要 `image_file`（已上传文件名，位于预览白名单目录）、`material`、`thickness_mm`（默认 3.0），可选（2026-11-06）`width_mm` / `height_mm`（成品尺寸，只给一边按原图比例，两边都给按比例放进该框）、`pixel_size_mm`（默认取材料库 raster 参数，0.05～1）、`line_interval_scale`（2026-11-16 技术书 r41：行间距 = 材料推荐 `pixel_size_mm` × 倍数，0.5～3，与 `pixel_size_mm` 只能给一个；给了才在产物里回显同名字段；前端质量预设快速 / 均衡 / 精细用 1.5 / 不提交 / 0.8，直通时不提交）、`image_mode`（`auto` 默认：中间灰度占比 >20% 按 Floyd–Steinberg 抖动，否则 Otsu 阈值；`dither`（Floyd–Steinberg）/ `threshold`；2026-11-16 技术书 r38 对齐 LightBurn Image Mode 另增 `jarvis`（Jarvis–Judice–Ninke，照片推荐）/ `stucki` / `atkinson` / `ordered`（8×8 Bayer）/ `halftone`（每格平均灰度画面积对应的圆点）/ `newsprint`（逐像素网点，固定 45°、4 像素网格）/ `sketch`（Sobel 高通，只刻明暗交界）/ `grayscale`（变功率））、`threshold`（0～255，仅阈值模式）、`work_area_width_mm` / `work_area_height_mm`（默认 100）。r38 可选项（缺省即旧行为）：`negative`（布尔，重采样后反相，透明背景反相后同样出光）、`pass_through`（布尔，直通：不重采样、网格即原图像素，行间距 = 成品宽度 ÷ 原图像素宽，须在 0.05～1 mm）、`dot_width_mm`（点宽补偿，≥0 且须小于实际行间距，每段出光两端各缩短一半）、`halftone_cell_mm`（网点网格 mm）或 `halftone_lpi`（每英寸网格数 2.54～508，二者只能给一个；都不给取 max(25.4/50, 4×行间距)，网格须在 2 个像素～10 mm）、`halftone_angle_deg`（-90～90，默认 45）、`gray_levels`（灰度级数 2～64，默认 16，含纯白不出光一级）、`gray_min_power_percent`（灰度最小功率 0～100%，默认 0，须小于本次功率；最黑一级 = 本次功率 `actual_params.laser_max_power`，其余级线性分布，同功率像素合并一段、S 值不超过本次功率）。2026-11-17（技术书 r45）起可选 `fill_options` 的扫描子集（键同 §3.2 / §3.4 的 `fill_options`）：`overscan_mm`（省略或 `auto` 按速度与加速度自动算，下限 1 mm，此前固定 1 mm）、`acceleration_mm_s2`、`bidirectional`（默认 true；false 时只单向出光）、`scan_offset_mm` / `scan_offset_table`（双向扫描偏移，蛇形扫描时每行逆着行进方向挪一半；偏移的一半不能超过过扫描，否则 400），没写的键取材料库光栅条目随材料保存的值；结果另含 `scan_options:{overscan_mm, overscan_auto, bidirectional, scan_offset_mm, scan_offset_table, from_material}`，`frame_mm.max_x` = 宽 + 2 × 实际过扫描。透明像素按白色不出光；都没给尺寸时按像素间距换算原图，超出幅面（宽度另留两侧各 1 mm 过扫描）自动等比缩小并返回 `auto_fitted=true` 与 `fit_note`，显式尺寸超出直接 400。产物另含 `pixel_size_mm`（实际行间距，直通时为推算值）、`image_mode`、`threshold`、`auto_fitted`、`negative`、`pass_through`、`dot_width_mm`，网点模式另含 `halftone_cell_mm` / `halftone_lpi` / `halftone_angle_deg`，灰度模式另含 `gray_levels` / `gray_min_power` / `gray_max_power`（S 值，1000 标尺）/ `gray_min_power_percent`；预览图为实际雕刻的黑白网格（灰度模式按级显示深浅）；`source_type=gcode` 需要 `gcode_file`；预览图与产物 `width_mm` / `height_mm` 由 `laser_core::gcode::preview_toolpath` 解析（2026-11-16 技术书 r41：跟踪模态 G0/G1/G2/G3、G90/G91、G20/G21、`G92`；XY 平面的 G2/G3 按 0.02 mm 弦高、每段 ≤10° 拆成折线，支持 `I/J`、`R`（负值取大于 180° 的弧）、顺 / 逆时针与整圆；单弧 ≤360 段、整份文件拆分点 ≤400 000，超出或半径无效、G18/G19 平面的弧只画首尾连线；`G53/G28/G30/G10` 行在预览里跳过。只影响画图与尺寸显示，发送前的范围复核仍用 `motion_extents`；两点对位生成的新刀路同样走这里）。生成产物、创建记录并冻结快照 | 400 `未指定图片素材路径` / `找不到指定的素材图片文件` / `未指定 G-code 刀路路径` / `找不到指定的 G-code 刀路文件` / `图片尺寸为0` / `成品尺寸 … 超出机器幅面` / `image_mode 只能是 …` / `negative 必须是 true 或 false`（`pass_through` 同）/ `dot_width_mm 不能为负数` / `dot_width_mm（点宽补偿）必须小于行间距 …` / `halftone_cell_mm 与 halftone_lpi 只能给一个` / `halftone_lpi 必须在 2.54 ~ 508 之间` / `网点网格须在 2 个像素 … ~ 10 mm 之间` / `halftone_angle_deg 必须在 -90 ~ 90 度之间` / `gray_levels 必须是 2 ~ 64 的整数` / `gray_min_power_percent …` / `直通模式的行间距 … 须在 0.05 ~ 1 mm 之间` / `pixel_size_mm 与 line_interval_scale 只能给一个` / `line_interval_scale（行间距倍数）须在 0.5 ~ 3 之间`；409 `device_busy` / `workflow_id_conflict` |
| `status` | 返回记录并叠加内存中的实时 `send_result`（`running, total_lines, lines_sent, target, phase, error, warnings?`） | 400 `workflow_id 不能为空`；404 `未找到 workflow` |
| `recent` | 最近记录列表 | |
| `confirm_send` / `send` | 必须 `confirmed=true`；不接受请求自带的 `start_from_line`（400 `resume_line_not_allowed`）；门禁顺序：记录存在 → 状态可发送（`sending/completed/failed/cancelled` 拒绝，`workflow_not_sendable`）→ 必须具备冻结 64 位 SHA-256 的有效快照（缺 SHA 的老数据一并拒绝，`confirmation_snapshot_missing`/`send_gate_rejected`）→ 物理文件 SHA-256 重算一致（`send_gate_rejected`）→ 设备连接（开连接即入活动任务表）、握手、`$CD`、物理二次 SHA-256 与行程复核、预检、行校验（§1 增补）→ 同设备无在途作业（409 `device_busy`；宿主退出时 503 `host_shutting_down`）。通过后后台逐行发送，响应 `status=sending` 与 `send_result`。可带 `offline_sd=true`（或 `send_mode=sd_offline`）走 SD 脱机模式（仅网口；固件不返回文件哈希时不执行 `$SD/Run`，任务记为 failed，2026-10-14）；SD 脱机上传端口取 `network_http_port`，其次 `http_port`，都缺省时为 80，给出的值必须是 1..=65535 的整数，否则在连接设备前返回 400 `sd_http_port_invalid`，不截断、不回落到 80（2026-11-05） | 400 / 404 / 409 / 503 |
| `frame` | 按预览走边框（2026-11-18，技术书 r51，给语音 / 小智用）：必须 `workflow_id` 与 `confirmed=true`（缺确认 400 `confirmation_required`）；带非 0 `power` 400 `framing_light_not_allowed`。门禁：记录存在（404 `workflow_not_found`）→ 状态可发送（同 `confirm_send`，`workflow_not_sendable`）→ 物理刀路 SHA-256 重算一致（`send_gate_rejected`）→ 记录有有效 `artifacts.frame_mm`（否则 400 `framing_not_available`，如导入 G-code）。范围只取 `frame_mm`，请求自带的 `min_*` / `max_*` / `outline` / `feed_rate` 一概不用；服务端固定 `relative_to_head=true`、`power=0`、`feed_rate=2000` 后交给 §3.13 `/api/device/framing` 同一套处理（参数校验、R3 授权、语音授权、设备占用、Idle 预检、活动作业 `framing-…`）。严格模式下按走边框（`ApprovalKind::Framing`）建审批，批准后按冻结的范围重放 `/api/device/framing`。成功 `result` 在走边框结果上加 `workflow_id`、`next_actions`、`hint`；不改记录状态，可再走或接着 `confirm_send`。内置语音（`laser_host::voice_tool_call`）不等走完：边框一开始走（出现新的活动作业 `framing-…`）就回话，走边框在后台照常执行，用户途中可暂停 / 继续 / 停止（§3.13，2026-11-18 r52）。走边框途中由语音 / 小智 / 网关对一份 workflow 调 `cancel`，且唯一的活动作业是还没在停止的走边框、这份 workflow 存在且在等待发送（不在启动或发送中）：停的是走边框（同 `stop_framing`），预览保留，响应 `detail.framing_stopped`；界面、空编号、正在启动或发送的任务照常取消。内置语音放弃等待（20 秒内没开始）后同一次请求不再开始，409 `short_job_abandoned`。门禁另含材料安全复核（400 `material_safety_blocked`，同 `confirm_send`）；`power: null` 视为没给 | 400 / 404 / 409 |
| `resume` | 仅在停机记录拥有设备直接证实的执行边界（`resume_boundary_source="device_hold_ln"` 且 `lines_completed==resumable_from_line>0`，由 `use_line_numbers: true` 加顺序 `N` 行号后经 `Hold:0\|Ln:K` 确认 `K-1`，禁止裸用入队 ACK `lines_sent` 或缓冲估算）且 `can_resume=true` 时才可续传（进度不能证实时取消后 `can_resume=false`、拒绝续雕）；丢弃客户端自报 `_origin_mpos`/`_artifacts`；要求 `confirmed=true`、**结构化规范连接目标（兼容 `127.0.0.1:23`、`tcp://127.0.0.1:23`、`COM3@115200`、`serial:COM3:115200` 等价写法）与原任务一致**、快照与物理刀路 SHA-256 在 `$X` 解锁前校验一致、记录未全量完成，且非 SD 脱机模式（`mode=sd_offline`、缺/异目标/缺断点/缺安全来源/缺协调 `MPos` 返回 400 `workflow_not_resumable`）。设备明确处于无物理报警引脚阻断的 `<Alarm>` 时才经 `verify_job_ready_for_resume_cancellable` 发送 `$X` 解锁（`Unknown` 禁止自动解锁）；续发前按首轮开工记录的 `origin_mpos` 与当前 `MPos` 重建 `0x18` 清空的 `G92` 坐标偏移（依赖 `G92`/`G91` 物理锚点却缺 `origin_mpos` 或 `current_mpos` 时阻断），在 `M5 S0` 下 `G0` 预定位至断点坐标并恢复 `F`/`M3`/`M4`/`G90`/`G91` 及首条切割行的模态 `S`，不篡改原刀路计数（2026-11-18） | 400 / 404 / 409 / 503 |
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
| test_type | string | 否 | `matrix`（默认，对齐 LightBurn Material Test）/ `interval`（对齐 Interval Test）；r40 起另有首次标定向导的测试图案 `scan_offset` / `overscan` / `kerf`（见下文）；其他值 400 `invalid_test_type`。2026-11-16 起 |
| speeds | int[] | 否 | 仅 `scan_offset`：1～6 个速度（mm/min，1～100000）；缺省为推荐速度的 0.5 / 1 / 1.5 倍（取 100 整） |
| square_mm | float | 否 | 向导测试方块边长，5～50；`scan_offset` 10、`overscan` 15、`kerf` 20 |
| fill_options | object | 否 | 仅 `overscan`：同生成请求的 `fill_options`（没写的键取材料库条目），决定右块的过扫描与两块共用的双向扫描偏移 |
| x_axis、y_axis | string | 否 | 列 / 行参数：`power`（默认列）、`speed`（默认行）、`interval`、`passes`；两轴相同或非法值 400 `invalid_axis`；`interval` 只用于光栅填充（`laser_mode=engrave` 且 `engraving_mode=raster`），否则 400 `axis_not_supported`。`test_type=interval` 时忽略 |
| interval_min_mm、interval_max_mm | float | 否 | 0.08、0.16；线距轴或线距测试使用，须在 0.01..2 且 min ≤ max |
| passes_min、passes_max | int | 否 | `passes`、`passes`+2（不超过 10）；遍数轴使用，须在 1..10 且 min ≤ max |
| power、speed | int | 否 | 不在轴上时的固定功率（0..1000）/ 速度（>0）；默认材料推荐值（无推荐 400 / 1500） |
| line_interval_mm | float | 否 | 不在轴上时的填充线距（0.01..2）；光栅填充默认取材料推荐的 `pixel_size_mm`，轮廓 / 切割不用 |
| run_order | string | 否 | `risk`（默认，从烧蚀风险最低到最高）/ `grid`（旧的网格顺序）；其他值 400 |
| labels | bool | 否 | true：用内置单线字体刻行列参数值与轴标题 |
| label_power、label_speed、label_height_mm | int、int、float | 否 | 默认风险最低那格的功率（不超过 300）、该格速度（不低于 1500 mm/min）、3 mm（线距测试 2.5 mm）；功率 0..1000、速度 >0、字高 1..10，否则 400 |
| cell_size_mm、cell_gap_mm | float | 否 | 12、4（线距测试 8、4）；1..50、0..50（上表 Python 时代的 `cell_size_mm/gap_mm` 默认值已不适用） |
| work_area_width_mm、work_area_height_mm | float | 否 | 2026-11-26（技术书 r65 / r68）：可选机器有效幅面；缺省或 `null` 时优先采用最近一次工作台握手记下的固件 X/Y 行程，无记录时回退默认 100 mm；提供或取自记忆记录时均须为 `(0, 2000]` 有限数字（越界或非有限数 400），矩阵或向导测试图案整体外框超出时返回 400 `矩阵约 W×H mm，超出当前行程 X×Y mm…` / `测试图案约 W×H mm，超出当前行程 X×Y mm` |
| surface | bool | 否 | 为“表面通用”条目标定（只允许雕刻，切割 400）；记录 `surface_entry=true`，选格时写回 `surface` 条目 |

Rust 实现：只生成不出光。区间字段缺省或为 -1 时以材料推荐参数为中心取 ±40%；`rows/columns` 限 1..25（线距测试固定 `rows=1`，`columns` 为方块数 2..20、默认 9，传其他 `rows` 400）。成功 `result` 为完整 calibration 记录：`calibration_id`（`cal-<ts>-<seq>`）、`status=prepared`、`cells[]`（`cell_number/id/row/column/x_mm/y_mm/size_mm/power/speed/passes/pixel_size_mm/line_interval_mm`，2026-11-16 起另有 `run_index`（第几个加工）、`x_label`/`y_label`（刻在样板上的值，功率为百分比））、`cell_count`、`width_mm`/`height_mm`（含标注的整体外框）、`gcode_file`、`gcode_sha256`、`confirmation_snapshot`、`time_estimate`、`web.gcode_download_url`，以及 `test_type`、`x_axis`、`y_axis`、`axes{x,y:{param,count,min,max,unit}}`、`fixed{power,speed,passes,line_interval_mm}`、`interval_min_mm/max`、`passes_min/max`、`cell_size_mm`、`cell_gap_mm`、`run_order`、`run_sequence`（按加工顺序的 `cell_number`）、`labels{enabled,height_mm,power,speed}`、`surface_entry`。`power_min/max`、`speed_min/max` 在该参数不在轴上时等于固定值。记录写入 `calibrations` 表（无 SQLite 时写内存表）。

**材料测试 / 线距测试（2026-11-16，用户决定对齐 LightBurn，“直接一起修改完在git推送”）**：`laser_core::material::material_test_cells` 按 `MaterialTestSpec` 生成单元格（`cell_number` 仍按行优先，`select-cell` 不变），`risk_order` 按单位面积能量 功率 × 遍数 ÷（速度 × 线距）从低到高排加工顺序（高速、低功率、宽线距、少遍数先刻），`layout_material_test` 在网格上方刻列值与列标题、左侧刻行值与竖排行标题（只标多于 1 格的方向），整体以激光头位置为左下角、坐标全 ≥ 0；`build_material_test_gcode` 先刻标注再逐格加工，单元格刀路写法与旧版相同，刀路超过 200 万行 400。`run_order=grid, labels=false` 时切割 / 轮廓矩阵的 G-code 与旧版逐字节相同；光栅矩阵的填充线距改为材料推荐值（旧版固定 0.2 mm）。线距测试是一排 8 mm 填充方块（默认 0.08 ~ 0.16 mm、9 块），每块上方刻线距值，目标是相邻扫描线刚好相接。确认、存储、发送门禁与普通矩阵完全相同。

**首次标定向导的测试图案（r40，2026-11-16，`laser_core::wizard_patterns`）**：与矩阵一样只生成、落盘、冻结 SHA-256、存为 calibration 记录（`wizard_pattern:true`、`next_actions:["send"]`、`rows=1`、`cells[]` 带 `overscan_mm` / `scan_offset_mm` / `x_label`、`square_mm`、`pattern{…, hint}`、`labels.enabled=false`），发送照常走 §3.8 的确认与完整门禁；`select-cell` 对这三种记录返回 400 `pattern_not_selectable`（结果由用户量好后在向导里填写）。记录另含 `matched_thickness_mm` 与如实的 `surface_entry`（2026-11-17 技术书 r43）：标明测试参数取自哪个材料库条目（雕刻在所选厚度没有光栅参数时回退到“表面通用”或最近厚度；切缝只认同厚度），向导的 `save_layer_options` 写回同一条目。

| test_type | 加工内容 | 参数来源 | 用户量什么、存到哪 |
|---|---|---|---|
| `scan_offset` | 一排实心方块，每块一个速度，偶数行往右、奇数行往左横扫，偏移 0，过扫描按速度与 `machine_motion` 加速度自动 | 材料库光栅条目的功率、线距（`power` / `line_interval_mm` 可覆盖）；无光栅条目 400 | 每块来回两行出光的错位（行行拖后填正数）→ 前端按速度并进本机填充设置的 `scan_offset_table`（跟机器走，不进材料库） |
| `overscan` | 两块实心方块：左块不过扫描，右块按当前过扫描；两块都用当前扫描偏移 | 同上；`pattern.overscan_mm` / `overscan_auto` 回显右块设置 | 右块两头是否发黑 → 发黑时建议填机器加速度或加大过扫描，`save_layer_options` 存进光栅条目 `fill_options.overscan_mm` |
| `kerf` | 沿 `square_mm`（默认 20）见方外框切 `passes` 圈，不做切缝补偿，`laser_mode=cut` | 该厚度的切割条目（功率、速度、遍数，可覆盖）；没有切割条目 400 | 切下来方块的边长 m → 切缝宽度 w = square − m，每边烧掉 w/2，切缝补偿 `kerf_mm = (square − m) / 2`（`laser_core::wizard_patterns::kerf_offset_mm`，±2 mm 内），`save_layer_options` 存进切割条目 `cut_options.kerf_mm` |

### 3.8 `/api/material-lab/send`

| 字段 | 规则 |
|---|---|
| calibration_id | 必填 |
| confirmed | **必须为 true**，否则 400 `发送测试矩阵需要 confirmed=true` |
| connection_mode | `network`（默认）或 `serial` |
| serial 时 | `port \| serial_port`（空 → 400 `请选择或填写串口号`）、`baudrate` |
| network 时 | `network_host`（空且无默认 host → 400 `请填写设备 host/IP`）、`network_http_port`、`network_telnet_port`、`network_timeout`；`network_transport` 固定 `telnet` |
| wait_for_response、run_in_background | true |

Rust 实现：与 `/api/workflow confirm_send` 同一套门禁——记录不存在 404 `calibration_missing`；`sending/completed/failed/cancelled` 状态 400 `calibration_not_sendable`；物理刀路 SHA-256 重算不一致 400 `send_gate_rejected`；设备不可达/握手失败 400 `device_unreachable`/`device_not_ready`；同一设备已有在途作业 409 `device_busy`。通过后由 JobWorker 后台逐行下发，响应 `status=sending` 与 `send_result{running,total_lines,lines_sent,target}`，终态由发送线程写回记录。`POST /api/material-lab/status {calibration_id}` 返回带实时进度的记录，供前端轮询；缺 id 400 `calibration_id 不能为空`，不存在 404 `未找到标定矩阵`。

### 3.9 `/api/material-lab/select-cell`

`calibration_id` 必填；`cell_number` int 默认 0；`thickness_mm` float 默认 0.0；`material`、`laser_mode`、`engraving_mode`、`notes` string 默认 ""。

Rust 实现：`cell_number` 必须在 1..cell_count；缺省的 material/thickness/laser_mode/engraving_mode 从 calibration 记录回填；把单元格的 `power→laser_max_power`、`speed→feed_rate`、`passes`（及线距 `line_interval_mm`→`pixel_size_mm`，材料库里 `pixel_size_mm` 即填充扫描线距 / 光栅点距）以 `source=calibration`（线距测试为 `interval_test`）经 `manage(save)` 写入材料库文件与 SQLite，并在记录上写 `selected_cell`（2026-11-16 起含 `line_interval_mm`、`surface`）。2026-11-16 起可选 `title`（条目标题，同 `manage(save)`）与 `surface`（写回表面通用条目；缺省时跟随记录的 `surface_entry`，切割 400）；默认 `notes` 写明测试类型与该格功率、速度、遍数、线距。成功 `result`：`{calibration_id, cell_number, selected_cell, materials, speech}`。

### 3.10 `/api/material-lab/manage`

`action` 必填，`save / save_layer_options / delete / import / export`，否则 400。

| action | 字段 | 结果 |
|---|---|---|
| export | 无 | 工具透传；`version` 恒为整数 1，`materials` 含内置预设（db-schema.md §3.6.1、§3.6.2） |
| import | `library_json \| params_json`（支持直接库对象或 `{library}` 导出包装）、`import_mode`（`replace` 默认 / `merge` 合并保留现有其他材料） | 工具 result + `materials` 全量快照 + `backup_path` + `import_mode`。`version` 校验严格：只接受 JSON 数值 `1`/`1.0`；入库前对材料名与别名执行 `validate_material_safety`、校验厚度 >0 与 `ParamCell::validate(1000, 100)`，并将非 1000 `power_scale`（如 255）等比折算归一为 1000 标尺。`backup_path` 只返回文件名 |
| delete | `material` 必填、`thickness_mm` float（数值等价匹配 `"2"` / `"2.0"`；缺省或 `0` 删除整材料；`surface:true` 或 `thickness_mm:"surface"` 只删表面通用条目，没有时 400）、`laser_mode` 默认 engrave、`engraving_mode` 默认 raster、`confirmed` **必须 true** 否则 400 `删除当前参数需要 confirmed=true` | 工具 result + `materials` |
| save | delete 的字段 + `surface`（bool，2026-11-16：存进表面通用条目 `thicknesses.surface`，忽略 `thickness_mm`，只允许雕刻，切割 400；`thickness_mm:"surface"` 等价）+ `title`（可选条目标题，字符串、去首尾空白后 ≤ 60 字，否则 400）+ `laser_min_power` 0、`laser_max_power` 800、`feed_rate` 1200、`travel_rate` 设置默认、`pixel_size_mm` 0.1、`threshold` -1、`passes` 1（数值字段在入库边界统一转为数字，无法转换或越界返回 400；材料名与别名均经 `validate_material_safety` 校验）、`aliases`（数组或中英文逗号分隔，Overwrite 时同步清理已移除旧别名）、`notes`、`source` 默认 manual、`prune_missing_thicknesses`（可选布尔，编辑多厚度规格对象时同步删除未在本次列表中的旧厚度）。**原子批量扩展（D9 已拍板）**：除单厚度参数外，允许传入 `thicknesses` 对象/列表进行多厚度一次性原子保存，后端在单个数据库事务内完成全部更新，失败整体回滚；禁止前端通过循环发送 N 次 HTTP 请求进行多厚度保存。r40 起可带 `fill_options`（雕刻条目）/ `cut_options`（切割条目）：给了就用（null 清除），没给就沿用该位置原条目的图层设置（改功率速度、选格写回不会丢） | 工具 result + `materials` |
| save_layer_options（r40） | `material`（名称或别名）、`thickness_mm`（数值等价匹配）或 `surface:true`、`laser_mode`（默认 engrave）、`engraving_mode`（默认 raster）、雕刻条目 `fill_options` / 切割条目 `cut_options`（对象；null 或空对象清除；放错条目 400）、`merge`（默认 false 整体替换；true 时把给出的键并进原有设置，值为 null 的键删除） | 只改已有条目随材料保存的图层设置，不动功率速度；条目不存在 400 `material_entry_missing`。成功 `{material, thickness_key, laser_mode, engraving_mode, fill_options|cut_options（保存后的值）, materials}` |

**材料库与 LightBurn Material Library 对照（2026-11-16）**：

| LightBurn | MOSS | 说明 |
|---|---|---|
| 材料 → 厚度条目（带 Description） | 材料 → 厚度键 → `engrave.raster` / `engrave.outline` / `cut` 参数格，每格可带 `title`（条目标题，存于参数格 `extra.title`）与 `notes` | 旧库 JSON 与 SQLite 无需迁移，`title` 走现有 `extra` 列 |
| No thickness 条目（表面雕刻） | 厚度键 `surface`（`laser_contracts::SURFACE_THICKNESS_KEY`），只存雕刻参数 | 雕刻推荐：同厚度有该模式参数 > 表面条目 > 最近厚度；切割仍要求真实厚度。`/api/ui/material-recommendation` 命中时 `match="surface"`、带 `title`；`/api/ui/material-options` 对有表面条目的材料加 `surface:true`（厚度列表不变；2026-11-20 起只有表面条目的材料也出现在工作台、画板与材料弹窗的下拉里，按表面通用参数雕刻，切割仍要求真实厚度） |
| 材料测试结果写回 | `select-cell` 写回功率、速度、遍数与线距（`pixel_size_mm`） | 线距即 LightBurn Interval |
| 库条目保存整套图层设置（r40） | 雕刻参数格可带 `fill_options`（`grouping`、`angle_deg`、`cross_hatch`、`bidirectional`、`overscan_mm`），切割参数格可带 `cut_options`（`kerf_mm`、`tab_count` / `tab_spacing_mm`、`tab_width_mm`、`tabs_skip_inner`、`lead_in_mm`、`lead_out_mm`、`lead_angle_deg`、`overcut_mm`），存于参数格 `extra`，值域与生成请求相同 | **双向扫描偏移（含速度表）与机器加速度跟机器走，不进材料库**（放进来 400）。生成请求没写的键回落到推荐条目的值，结果 `fill_options.from_material` / `cut_options.from_material` 列出取自材料库的键；导入的库同样校验 |
| Load / Save / Merge `.clb` | JSON 导出、导入（`replace` / `merge`） | **不支持 `.clb`**：LightBurn 私有 XML，单位与字段（mm/s、百分比功率、图层模式）需人工换算，未做导入器 |
| Assign（复制设置到图层）/ Link（图层跟随库条目） | 无图层模型：每次预览按当时的材料库取推荐参数，预览后参数冻结进确认快照 | 行为接近“预览时 Link、确认后 Assign”；没有可链接的图层，故 Link **不适用** |
| 每台设备一份材料库 | 单一材料库，参数统一按 0~1000 标尺存储、发送时按设备 `$30` 换算 | 多设备分库**暂不适用**（当前一台机器的使用方式）；不同功率机器请用不同材料名或导入 / 导出切换 |

### 3.10b `/api/time-calibration`（工时自校准，r40）

加工正常完成后（`completed`，非取消 / 失败 / 急停，非断点续雕，非 SD 脱机，实际用时 ≥ 20 秒，记录里有预览时的原始估算）发送线程计算“实际 / 预估”比值（超出 0.2～5 视为异常不计），按机器目标（连接描述：串口 `端口@波特率`、网络 `主机:端口`）写入运行目录 `settings.json` 的 `laser_time_calibration:{目标:{ratios:[最近 10 次], updated_at}}`（最多 8 台，去掉最久没更新的）；完成结果 `send_result.time_calibration` 回显 `{recorded:true, ratio, samples, factor}` 或 `{recorded:false, reason}`（`not_completed` / `resumed` / `sd_offline` / `too_short` / `no_estimate` / `outlier` / `no_runtime_dir` / `settings_write_failed`）。

满 2 次后校准系数 = 最近比值的中位数（限制 0.5～3）。之后的文字、图片、画板、G-code、对位、反馈重生成与标定（含向导图案）预览的 `time_estimate` 增加 `raw_estimated_seconds`（原始估算）与 `calibration:{factor, samples, source:"recent_jobs"}`，`estimated_seconds` / `estimated_minutes` 改为校准后的值（`laser_seconds` 等分项保持原始值）；预览用哪台机器：请求里给了连接字段（串口须给端口、网络须给主机）用它，否则用记住的连接（n73），再否则用最近校准过的那台；该机器样本不足时不改动。

- `GET`：`{targets:[{target, samples, factor|null, active, updated_at}], min_samples:2, max_samples:10, min_actual_seconds:20, factor_range:[0.5,3]}`。
- `POST {action:"reset", target?}`：清除该台（不给 target 时全部）；其他 action 400；局域网连接只读（`lan_read_only`）。

### 3.10c `/api/engrave-route`（雕刻路线全局开关，r45）

用户决定（2026-11-17）：“生成策略……下面加一个开关用来转图片还是矢量的，工作台和创作画板都要有并且两个开关是内部是一个东西……语音说的说话也是这样生成的，不能两边不同步”。一个值，存运行目录 `settings.json` 的 `laser_engrave_route`（`"vector"` 默认 / `"image"`，缺省或无效按矢量）；工作台与画板上的开关只是它的两处入口。

- `GET`：`{route}`。
- `POST {route:"vector"|"image"}`：保存并返回 `{route}`；其他值 400；局域网连接只读（`lan_read_only`）；语音与 AI 工具没有改它的入口。
- 生成时的用法：§3.4 文字任务请求没写 `engrave_route` 时按它（工作台不传、始终跟开关；语音 / AI 生成同一条）；§3.2 画板请求没写 `mode` 时按它（矢量 → `vector`，转图片 → `raster`）。采用反馈建议重新生成时同样按当前开关。

**文字对刀起点 `/api/text-origin`**（r48，2026-11-17 用户决定：“就按照这么做”——把对刀起点做成后端全局设置，语音没明确说起点时按它）：存运行目录 `settings.json` 的 `laser_text_origin`（`"center"` 默认 / `"lower_left"`，缺省或无效按中心）。`GET` 返回 `{origin}`；`POST {origin}` 保存并返回 `{origin}`，其他值 400，局域网只读（`lan_read_only`）。§3.4 文字任务请求没写 `origin` 时按它（工作台显式发送当前选择，即它的值；语音 / AI 没说起点就不填），写了就按请求。创作画板与图片任务固定左下角，不受影响。工具 `laser_workflow_tool` 的 `origin` 不再写死默认值，说明为“用户没说就不填”。

### 3.11 `/api/material-lab/recent`

GET 与 POST 都接受；忽略请求体（仍受 64 KiB 限制）。从 workflow 按 `updated_at` 倒序取第一个含有效参数的记录：优先提取 `artifacts.actual_params`（含 `manual_params` 临时自定义参数并归一到 1000 标尺）与 `text_fill`/`mode` 对应的雕刻策略，无 `actual_params` 时回退查材料库推荐参数。
成功 `result`：`{workflow_id, material, thickness_mm, laser_mode, engraving_mode, params}`。
失败 400：`最近的激光 workflow 中没有可保存的完整材料参数`。

### 3.12 `/api/device/framing`

真实驱动机器沿外接矩形走边框，用于校对材料摆放位置。2026-11-16 起可选 `outline: [[x, y], …]`（橡皮筋走边框，对齐 LightBurn Rubber Band Framing）：沿这个多边形绕一圈（取预览返回的 `frame_hull_mm`），3 ~ 256 个有限数字点且都在 `min_*` / `max_*` 范围内（容差 0.01 mm），否则在审批与连接设备之前 400 `framing_params_invalid`；`outline` 进入严格模式审批摘要，摘要写“沿图形外轮廓 N 个点”。可选 `relative_to_head: true`（2026-11-06，AI 预览窗“先走边框”）：先 `G92 X0 Y0` 把激光头当前位置设为原点，`min_*` / `max_*` 可为负（取预览返回的 `frame_mm`），结尾回到起点；该字段进入严格模式审批摘要。字段：`min_x`、`min_y`（默认 0）、`max_x`、`max_y`（默认 100）、`feed_rate`（默认 2000）、`power`（默认 **0，不出光**）与 §1.5 连接字段。`power` 必须是 0..=1000 的 JSON 整数（G-code S 标尺，下发时按设备 `$30` 换算），`feed_rate` 必须是 1..=20000 的 JSON 整数（mm/min）；超出 `i32`、越界、小数或字符串一律在审批与连接设备之前返回 400 `framing_params_invalid`，不再截断回绕（此前 `4294968096` 会变成 S800），严格模式审批摘要与实际下发因此是同一个已校验的值（2026-11-05）。流程与完整发送一致（2026-10-14 起）：生成 framing G-code → 同设备在途作业 409 `device_busy` → 打开传输 → 握手 → `$CD` → Idle 预检（失败 400 `device_preflight_failed`）→ 登记为活动作业 `framing-…`（`/api/jobs/stop-targets` 可见，顶栏急停、`/api/safe-action` 带 `job_id` 可停）→ `JobWorker::send_lines`。成功 `result`：`{message:"走边框完成", job_id, laser_on, bounds}`；失败 400（生成、连接、握手、预检、行校验），500 `framing_failed`（执行失败，已补发 0x18），409 `framing_cancelled`（被急停或取消）；后两种保留为“待确认停止”目标。不经 workflow 快照，也不需要 `confirmed`，改动时按发送链路对待。严格模式下按 R3 处理（授权规则同 §3.6）。

内置语音等待走边框开始或收到终态回执共用 20 秒期限。若收到终态通知却在剩余时间内收不到回执，回复“走边框没有执行完，结果未知，请看电脑上的 MOSS”。

### 3.13 `/api/safe-action`

请求体为 `laser_contracts::LaserSafeActionArgs`：`action` 必填，可选 `confirmed`、`dry_run`、`x`、`y`、`feed_rate`、`power` 与连接字段。动作集合见 [开发者安全入口](safety.md) §2；`status` 与进给/功率倍率不需确认，其余需 `confirmed=true`。成功 200，失败 400，响应体 `{success, result, detail}`。MCP `laser_safe_action_tool` 在有宿主时转发到本路由。严格模式下，除 `status`、`emergency_stop`、`soft_reset`、`laser_off`、倍率复位与降低外的动作按 R3 处理（授权规则同 §3.6，判定函数 `laser_contracts::safe_action_requires_approval`）。

2026-10-10：HTTP 停止/查询请求可另带 `job_id`，仅限 `emergency_stop`、`soft_reset`、`status`，按登记任务使用其连接；编号未知或与显式目标不一致时失败，不改用其他设备。无编号时优先匹配显式连接目标；多个活动任务且无目标时拒绝随意选择。无活动任务且无编号时仍可通过明确的连接配置发送停止，不能依赖任务列表存在。`dry_run=true` 不写入活动任务的实时字节。成功只证明请求发送，不保证设备物理停止。

2026-11-18（技术书 r52）：走边框途中的 `pause` / `resume` / `stop_framing`，可带 `job_id`，不带时取唯一的活动作业（多个时拒绝）；只作用于走边框作业 `framing-…`，正式加工返回 `detail="framing_control_not_supported"`，没有活动作业返回 `detail="no_active_framing"`。`pause` 在原任务通道写进给保持 `!` 并把 `paused` 记进停止目标；`resume` 写 `~`，必须 `confirmed=true`（否则 `detail="confirmation_required"`），严格模式下按 R3；`stop_framing` 先写 `!` 再取消作业，作业线程等停稳后发 0x18，激光头停在半路，与急停一样转为待确认停止。`pause`、`stop_framing` 属降低风险动作，严格模式不追加条件。`resume` 在授权之前先确认（400 `confirmation_required`）、先找到正在走且未在停止的走边框（否则 400 `no_active_framing`，不建审批），把它的 `job_id` 写进请求（进入审批摘要），请求没带连接时补上这次作业的连接作为审批目标，摘要写“继续走边框”；批准重放只作用于那个 `job_id`。三个动作都核对显式连接目标与作业设备一致（否则 `detail="framing_target_mismatch"`），支持 `dry_run`；已在停止的走边框不能再暂停或继续。`stop_framing` 的进给保持写不出去时如实失败并提示急停。停止目标的 `paused` 以控制器状态报告为准（`Hold` / `Door` 为暂停，`Run` / `Idle` 为非暂停）。

2026-11-05：`action` 先去首尾空白再转小写后匹配（与 `laser_tools::execute_laser_safe_action_with_gate`、`safe_action_requires_approval` 一致），包括 `job_id` 允许的动作检查；`Emergency_Stop`、`EMERGENCY_STOP` 在作业运行中同样走原任务通道写出实时 0x18。

2026-11-27（技术书 r73）：① 进给/功率倍率类实时动作（`feed_override_*` / `feed_*` / `power_override_*` / `power_*`，即 `realtime_action` 中 `alters_alarm_state=false` 的动作）**必须命中正在运行（未进入取消态）的任务，并经该任务原来的设备通道发送实时字节，绝不新开连接**。当前没有运行中任务返回 400 `detail="no_active_job"`；显式设备目标与运行中任务不一致返回 400 `detail="override_target_mismatch"`；存在多个运行中任务且未指定目标返回 400 `detail="override_target_ambiguous"`。严格模式下对需要桌面审批的倍率提高动作（`*_plus_10`），创建审批前先校验命中运行中任务并写入内部 `override_job_id`（非审批调用方传入该字段一律剥离）与该任务连接（无运行中任务不建废审批）；桌面批准重放时按执行那一刻的运行态重新判断——原任务已结束则返回 `no_active_job` 或 `override_job_ended`，不向空闲控制器或后续新任务发倍率字节。急停与软复位不受影响。② `absolute_move`（含工作台“回工作原点”）改为下发 `M5` / `G90` / `G1 X<x> Y<y> S0 F<feed>`（此前 `G0 ... F` 在 GRBL 中会忽略进给速度 `F`），保证按设定速度关光移回。

### 3.13a 两点对位 `/api/registration/*`（2026-11-16，对齐 LightBurn Print and Cut，无摄像头）

用户决定（2026-11-16）：对齐 LightBurn / 业界做法，“直接一起修改完在git推送”。流程：材料上已有两个实物标记（印好的十字、木牌两个角等）→ 工作台点动激光头（红光点）对准第 1 个标记 → 记录 → 对准第 2 个 → 记录 → 生成对位预览 → 照常走边框与确认发送。与 LightBurn 不同，MOSS 的任务都相对开刻时激光头位置（`G92`），不使用机床坐标，因此**新任务的开始位置固定为第 2 个标记点**（记录第 2 点时激光头就停在那里），不需要先记录或回到原来的起点。

**`POST /api/registration/read-position`**：请求体为 §1.5 连接字段（也接受 `/api/safe-action` 的 `host` / `telnet_port` / `port` 写法）；内部按 `/api/safe-action {action:"status"}` 执行，**只写实时字节 `?`，不发任何 G-code 行、运动或出光**，请求里夹带的 `x`、`y`、`confirmed` 等字段一律丢弃。非 R3，严格模式下不需要审批。成功 `result`：`{state, frame:"mpos"|"wpos", position:{x,y}, mpos:{x,y,z}|null, wpos:{x,y,z}|null, raw}`（优先 `MPos`；固件只报 `WPos` 时用 `WPos`，两点必须同一种）。失败：409 `registration_device_busy`（该设备正在加工或上一任务停机未确认）、409 `registration_not_idle`（不是 `Idle`，如点动还没停）、400 `registration_status_failed`（连不上、无响应）、400 `registration_position_unavailable`（没有坐标字段）。

**`POST /api/registration/preview`**：

| 字段 | 类型 | 规则 |
|---|---|---|
| workflow_id | string | 必填，原任务（任何带刀路文件的 workflow，含导入 G-code）；正在加工 409 `registration_source_busy` |
| design_points | `[{x,y},{x,y}]` | 必填，设计上的两个参考点，坐标与预览 `frame_mm` 相同（相对原任务开刻时激光头位置，mm） |
| measured_points | `[{x,y},{x,y}]` | 必填，记录到的两个标记点位置（`read-position` 的 `position`），第 2 个即新任务开始位置 |
| position_frame | string | 可选 `mpos`（默认）/ `wpos`，只记录 |
| allow_scale | bool | 可选，默认 `false`：只旋转平移；`true` 时另按实测距离等比缩放 |
| machine_motion | object | 可选，同 §3.4，只影响工时估算 |

算法（`laser_core::registration`）：`fit_two_point` 求相似变换（不含镜像）；两点相距不足 10 mm 400 `registration_points_too_close`；不允许缩放时实测距离与设计距离相差超过 5% 400 `registration_scale_mismatch`（多半点错了标记），按中点对中点做最小二乘，残差为距离差的一半；允许缩放时比例须在 0.8 ~ 1.25，否则 400 `registration_scale_out_of_range`；坐标缺失或非数字 400 `registration_points_invalid`。原任务刀路先按冻结的 SHA-256 重算比对（不一致 400 `send_gate_rejected`，不拿来变换），材料安全门同预览（`material_safety_blocked`）。`transform_gcode` 跟踪 `G90/G91`、`G92` 原点与模态，把每条移动换成绝对坐标并同时写 X、Y，圆弧 I/J 随之旋转缩放、`R` 乘缩放比例，方向不变；F、S、M、Z 原样保留；开头写 `G21` / `G90` / `G92 X0 Y0`，原文件的 `G92` 行只用于跟踪、不输出。以下情况 400 `registration_gcode_unsupported` 并写明行号：`G20` 英制、`G53` `G28` `G30` `G10` `G54`~`G59`、`G18` `G19` 等其他 G 代码、额外轴或 `K`、`$` 系统命令（含 `$J=`）、变量与表达式、相对模式下或改过 Z 原点后的 Z 移动、没有任何 X/Y 移动。扫描填充、光栅也按同一几何变换（整行连同两头过扫描一起旋转），扫描方向随之倾斜。

成功 `result` 是**新的** workflow 记录（原任务与原文件不变）：`workflow_id`（新）、`status=preview_ready`、`source_type="gcode"`、`registered_from`（原任务）、`registration{source_workflow_id, source_gcode_file, source_gcode_sha256, design_points, measured_points, position_frame, start_point:"mark_2", allow_scale, rotation_deg（逆时针为正）, scale, scale_applied, design_distance_mm, measured_distance_mm, distance_ratio, residual_mm, offset_mm{x,y}（原起点落在第 2 个标记点的哪里）, moves, arcs}`（同时写入 `artifacts.registration`）、`artifacts`（新文件 `reg-….gcode` 的 `gcode_file` / `gcode_sha256` / 预览图 / `time_estimate`，另加按变换后刀路算的 `frame_mm`、`frame_hull_mm`，原任务的 `actual_params`，`origin="registration_mark_2"` 与 `origin_note`）、`confirmation_snapshot`（新文件与新 SHA）、`origin`（发起方）与 `web`。发送走 §3.6 `confirm_send` 全部门禁：必须 `confirmed=true`，物理文件 SHA-256 重算、握手、预检、行程复核（`job_exceeds_travel`）照旧，严格模式下按 R3 授权。生成本身不连接设备、不算 R3。

**画板任务对位**（2026-11-16，技术书 r42）：原任务 `source_type="draw"`（`/api/draw/preview` 生成）时，结果另带 `draw_lab{job_summary, content_fingerprint, registered_from}`：`job_summary` 沿用原画板任务的作业摘要（材料、功率、速度、遍数、`can_send`），`actual_width_mm` / `actual_height_mm` 换成变换后 `frame_mm` 的宽高、`estimated_display` 换成新工时，另加 `registered:true`；`content_fingerprint` 是新刀路的 SHA-256（与原画板图案不同；同一份对位结果再次发送时画板页照常要求勾选“已移动材料或原点”）。`web` 另带原任务的 `scene_download_url`。其他来源的结果不带 `draw_lab`，字段与上文一致。

### 3.13b 低功率点射 `POST /api/device/pointer-fire`（2026-11-16，对齐 LightBurn “Fire”，技术书 r42）

用户决定（2026-11-16）：“二、值得做的优化（按收益排序）帮我优化一下吧，代码健康也完成一下吧”——补齐两点对位，没有红光指示的机器在对准标记时可以原地打一个极弱的点看清激光头位置。**会真实出光**，按发送链路对待。

| 字段 | 类型 | 规则 |
|---|---|---|
| confirmed | bool | 必须是 JSON `true`，否则 400 `confirmation_required` |
| power | int | 可选，默认 `10`；G-code S 标尺 0~1000 上的 **1..=10**（≤1%），下发时按设备 `$30/1000` 换算（`$30=255` 时 S10 → S3） |
| duration_ms | int | 可选，默认 `100`；**10..=200** 毫秒 |
| 连接字段 | — | §1.5 |

`power` / `duration_ms` 必须是范围内的 JSON 整数，越界、小数、字符串、溢出一律在连接设备之前 400 `pointer_fire_params_invalid`，不截断；请求里的 `x`、`y`、`feed_rate` 等运动字段被忽略，激光头不移动。

调用方：只接受工作台身份（桌面会话、本机回环、局域网配对）。内置语音、小智接入点 / MCP 桥、网关与审批重放（`CallerContext::Voice` / `McpBridge` / `Gateway` / `Approval`）**不论严格模式开关一律 403 `pointer_fire_not_allowed`**，不连接设备；`ToolCatalog` 与 `laser_safe_action_tool` 都没有点射动作。启用了桌面会话（桌面版）时，不带会话的本机回环请求不论严格模式开关一律 403 `desktop_session_required`（2026-11-17 技术书 r43，与确认停机、操作员设置一致）。严格模式开启时只有桌面会话放行，本机回环与局域网配对 403 `strict_mode_blocked`（尽力记审计，类别记为 `safe_action`），**不创建审批**——事后批准的一次点射对“此刻对准标记”没有意义。

下发的 G-code（`safe_action::pointer_fire_lines`，以 S10、100 ms 为例）：

```
M5 S0
G1 F100
M3 S10
G4 P0.100
M5 S0
G0
```

`G1 F100` 不带坐标、不运动，只把模态切到直线：GRBL / FluidNC 激光模式（`$32=1`，握手要求）只在 G1/G2/G3 模态下让激光输出，G0 模态下 `M3` 不出光。必须用 **`M3` 恒定功率**：`M4` 动态功率按实际速度折算，原地不动时输出为 0，打不出点。`G4` 停留后 `M5 S0` 关光，最后 `G0` 把模态切回快速移动。这几行不到 50 字节，字符计数流控会在 `M3` 生效前把关光行一并写进控制器接收缓冲区，所以主机在点射中途断线也不会让激光一直亮着；之后照常 `G4 P0` 同步并等 Idle。

流程与 §3.12 走边框相同（共用 `run_short_device_job`）：同设备在途作业 409 `device_busy` → 打开传输 → 握手 → `$CD` → Idle 预检（Alarm / Run / Jog / Hold / Door 等 400 `device_preflight_failed`，不出光）→ 登记为活动作业 `pointer-…`（`/api/jobs/stop-targets` 可见，顶栏急停、`/api/safe-action` 带 `job_id` 可停）→ `JobWorker::send_lines`。成功 `result`：`{message:"点射完成，激光已关闭", job_id, power, duration_ms}`；500 `pointer_fire_failed`（执行中出错，已补发实时 0x18）、409 `pointer_fire_cancelled`（被急停或取消）；后两种保留为“待确认停止”目标。0x18 软复位会关掉激光输出，但软件急停不代表物理停机，界面提示必要时按实体急停。未用真实机器验证点射的可见度与 M3 原地出光行为。

### 3.14 `/api/jev/status`、`/api/jev/reveal`、`/api/jev/test`、`/api/jev/feedback`

Jev 是可选的加工反馈判断服务，只接入 OpenRouter。固定模型 `typesafe/jev-1.13`，固定 `POST https://openrouter.ai/api/alpha/decisions`，Bearer 认证；请求含 `model`、`state`、`questions.feedback_type.{type:"choice",instructions,criteria}`，响应读取 `answers.feedback_type.{type,choice,confidence}`。不使用 Chat Completions 或 TypeSafe 直连接口，不跟随重定向，超时 5 秒，响应上限 64 KiB。协议依据 OpenRouter 官方 `openapi.json`（2026-09-22 核对）。

`GET /api/jev/status` 返回 `enabled`、固定 `endpoint`、`model`、`provider`、`configured` 和 `masked_key`（仅在受信任操作者会话下返回脱敏掩码，非桌面或局域网会话下 `masked_key` 为空串），不返回完整 Key。`POST /api/jev/reveal` 返回完整 Key，`POST /api/jev/test` 使用已保存的 OpenRouter Key 测试 Decisions 契约（不连接设备）；两者无 Key 均为 404，且在桌面会话启用时统一要求 `X-Moss-Desktop-Session`，非桌面回环进程、语音/MCP 桥与局域网均以 403 `desktop_session_required` / `lan_read_only` 拒绝。旧 TypeSafe Key 不能用于 OpenRouter，升级后需在设置中替换为 OpenRouter Key。`POST /api/feedback` 支持读取 `voice.builtin_local.jev_model`（含本地 Laya 或指定 OpenRouter 模型），并在未单独保存 `jev_api_key` 且内置语音大模型配置为 `https://openrouter.ai` 时安全复用 `voice.llm.main` 密钥。

`POST /api/feedback`（兼容别名 `/api/jev/feedback`）接收 `{workflow_id|task_id, feedback_text}`。必须关联已有 workflow，反馈长度为 1..2000 字符。原参数读取 `artifacts.actual_params`；无此快照的旧任务或上传 G-code 必须显式提供 `laser_max_power`、`feed_rate`、`passes`、`power_scale`，只提供建议，不能自动采用。参数须为有限数字：`0<power<=power_scale`、标尺 1..100000、速度 100..6000 mm/min、次数为 1..5 整数。新生成预览使用 1000 标尺，反馈参数中的功率、速度须为整数；超限或缺失返回 400，显式参数与实际快照冲突返回 409，不猜默认参数。

分类：`too_dark/too_light/not_cut_through/rough_edge/broken_line/acceptable/unknown`。模型只分类；本地确定性规则转换为有界参数。置信度低于 0.8 时按 `unknown` 处理（保守策略值，未经实机校准）。`acceptable/unknown` 不调整。服务未启用、缺 Key、HTTP/协议错误时由 API 统一回退本地规则；否定、多个问题及不明确反馈不调整。MCP 与 Web/桌面共用该结果，不再透传模型原始 JSON。

成功 `result` 包含 `feedback_id/workflow_id/feedback_text/created_at/source/fallback_reason/feedback_type/decision/original_params/suggested_params/adopted_params/adopted_workflow_id/can_adopt/speech`。`source` 为 `openrouter`、`local_jev`（本机 OneJev / Laya 网关分类，`decision.model` 为本地模型编号，2026-11-07）或 `local_rules`。首次仅保存建议，`adopted_params=null`；不保存 Key、原始网络响应、文件路径到模型输入。历史放在 workflow payload 的 `feedback_history`，通过 RuntimeStore 原子合并，保留当前作业状态；`GET /api/feedback/history?workflow_id=...` 读取历史。`jobs` 表仍无新增写入方，不伪造独立作业。

`POST /api/feedback/accept` 接收 `{workflow_id,feedback_id,confirmed:true}`，支持 `source_type` 为 `text`、`draw` 与 `image`（`image` 原图工作流保留 Floyd–Steinberg 抖动 / 阈值、`pixel_size_mm`、`source_image_file` 与 `frame_mm`，并清除原任务 `manual_params` 后应用建议参数，2026-11-12），生成独立 `preview_ready` workflow，返回更新后的反馈记录及 `adopted_workflow_id`。成功保存新预览后才记采用参数，重复请求返回已采用记录。不会改原文件、直接修改材料库或发送硬件。新文件实际使用建议的功率、速度和次数，并重新冻结 SHA；发送仍须对新 workflow 独立 `confirm_send` 和全部既有门禁。过期或加工中的任务返回 409。MCP 采用通过现有 `regenerate_text_laser_task_tool(task_id,feedback_id,confirmed=true)`，不新增工具。

新 workflow 与原反馈的采用状态在同一 SQLite 事务（内存模式同一把锁）提交；失败不会留下可查询的孤立任务。普通 workflow 状态写回保留数据库中现有反馈历史，避免旧快照覆盖历史。新任务另有 `feedback_origin`，界面切换后仍可显示本次来源反馈。`feedback_params` 是内部生成参数，公开 text/draw/workflow 请求不能直接传入，必须走采用接口。画板当前支持光栅（`raster`）与轮廓描边（`outline`）两种实际生成策略，摘要按实际生成的刀路策略显示，不把未独立实现的 `cut` 标签当成矢量切割策略。

### 3.15 `/api/settings` 与 `/api/system/check-update`

`{web_port, mcp_endpoint, jev_enabled, jev_api_key, jev_delete_api_key, lan_access, auto_check_update, check_prerelease, process_trace_enabled}`（`process_trace_enabled` 须为布尔，保存后记录器立即重读开关，n105）。**`POST /api/process-trace {action:"status"|"clear"}`**（n105，2026-11-22）：与 `/api/settings` 同一权限检查（语音、MCP 桥、审批、网关、局域网一律 403，桌面会话启用时要求 `X-Moss-Desktop-Session`），返回 `{enabled, dir, custom_dir, available, error, bytes, days, retention_days:30}`（n110 起不按大小限制，去掉 `max_bytes`）（`dir` 为本机绝对路径，只在这里给出）；`custom_dir` 为用户自选的最终记录目录（默认时为 `null`），`available` / `error` 报告当前目录能否写入及原因。`clear` 只清理当前目录内 MOSS 的日期记录，保留根目录、无关文件与链接，其他 `action` 返回 400。**自定义保存位置（2026-11-23，r62）**：默认仍为 `<运行目录>/logs/process-trace`；只允许受信桌面主窗口通过 `pick_process_trace_dir` / `reset_process_trace_dir` 修改 `settings.json` 顶层 `process_trace_dir`，`/api/settings` 带该字段一律拒绝，公开 `/api/system/info` 不返回路径。原生选择框选定现有文件夹后使用其 `moss-process-trace` 子目录（已同名不重复嵌套），该键保存最终目录；已有非空且未标记归属的同名目录拒绝接管。取消选择不改设置。`open_process_trace_dir` 只打开当前目录，不接受任意路径；目录尚未生成时明确提示。设置保存成功后新记录立即使用新位置；已经开始的一组记录与附件固定在原位置写完，同一次 HTTP 调用的开始和结束保持同目录。旧记录不迁移、不继续参与当前目录的自动清理。自定义根缺失或不可写时暂停记录，不重建挂载父路径、不静默回退；恢复可写后继续，缺失期间的内容不补写。目录命令在移动端不支持。记录格式（n106 起为统一时间线，n110 起按真实先后、不限大小）：`<运行目录>/logs/process-trace/<本地日期>/timeline.jsonl` 一行一条 `{seq, run, at, at_ms, domain:"voice"|"laser"|"diag", flow, kind, step, dropped_before?, ...}`（取时刻与入队在同一把锁里完成，后台单写线程按入队先后编 `seq`，所以文件里的先后、`seq` 与 `at` 三者一致；`run` 为 MOSS 启动时刻；`flow` 为所属流程如 `语音·2233·第3轮` / `语音·2233·后台` / `激光·wf-12` / `诊断·过程记录`；`step` 为一句中文说明这一步在干什么，JSON 其余字段保留原始细节、不截断；只有写线程起不来时没能入队的条数会以 `dropped_before: N` 写在下一条成功的记录上），同目录 `timeline.txt` 一行一步可直接阅读（`HH:MM:SS.mmm #000153 [流程] 说明`）；附件在同日 `{voice,laser}-files/`，记录里的文件字段是相对该条记录所属保存根目录的路径，附件真没写成时补一步 `attachment_failed`。语音 `kind`（n110 补齐）：`service_start`（端口、对话模型与各类本地模型选了什么、装没装、加载上没有）/ `service_stop`、`helper_start` / `helper_exit` / `helper_stderr`（本地模型程序与语义判停程序的启动、退出与标准错误输出，去疑似密钥、不截断）、`listen_start` / `listen_stop`、`vad_engine`（判停实际方式与改用能量判停的原因）、`denoise`（这一路判停 / 识别音频有没有先降噪、用哪个模型或没建起来的原因，来自辅助程序协议 8 的 `Opened{denoise}`）、`speech_began` / `speech_end`、`vad_skipped`、`kws_hit`、`interim`（边说边识别的中间文字）、`follow_probe`、`user_audio`（WAV，上行采样率）、`asr`（原文、同音纠正后、命令词改判后分开记）/ `asr_ignored`、`smart_turn`（语义判停概率与耗时或没用上的原因）、`hold_decision`、`user_continued`（n112 判停复盘：上一句判停后用户又出声时记下当时阶段 `thinking` / `speaking`、是否与前半句合并 `merged`、已执行工具来不及合并 `too_late`、原话 `text`、句尾规则与最终等待 `text_wait_ms` / `total_wait_ms`、是否已放行 `released` / `since_release_ms`、距开播 `since_playback_ms`、开头长度 `head_ms` / 其中人声 `voiced_head_ms`、估算停顿 `pause_estimate_ms`，与该轮 `hold_decision` 同一 `turn`）、`jev_hold_check` / `jev_hold`、`fast_stop`（带触发来源）/ `say` / `report_input`、`jev`、`llm_request`（请求发出时：完整消息与工具定义）、`llm_round`（回复完整时：`round`、回复文字与工具调用、首字与纯大模型耗时）、`tool_call`（含快路径直接调用，完整参数与结果）、`tts_start` / `tts_ready` / `tts_failed`、`sentence_play`、`interrupt`、`turn_end`（`outcome` 含 `replaced`、`spoken`、`reply`、`tts_file` 下行采样率 WAV，不限时长）、`timing`（阶段名配中文）、`background_llm_start` / `background_llm`、`vision_start` / `vision`；诊断 `kind:"event"`（会话内事件归到对应设备与轮次）。激光 `kind`（r61）：`api_begin`（开始处理：发起方与请求）与 `api_call`（处理完：HTTP 状态、响应与耗时），两步用同一个 `call` 编号；急停、软复位等 `/api/safe-action`、`/api/jobs/acknowledge-stop` 与 `/api/workflow` 取消先执行、后补记两步（`received_at` 为收到时刻），执行前不读开关、不抢记录锁；打开记录开关的那次调用补记开始一步；开始那步记下了，结束那步即使开关刚被关掉也照写。GET 与 `status`/`recent`/`list` 等只读动作、缺省 `action` 的 `/api/workflow`、`/api/material-lab/{status,recent}`、`/api/registration/read-position`、`/api/process-trace`、`/api/jev/reveal`、`/v1/auth/*` 不记；`/api/draw/state`（画板自动同步）只记大小。脱敏只换凭据本身：字段名按词判断（`_`/`-`/驼峰/缩写切词，含 key、token、secret、password、passcode、pin、authorization、auth、credential、cookie、sig、signature、jwt、otp 及复数）的非空值整个记为 `[已隐藏]`；文字里 `名字=值` / `名字: 值` / `"名字": "值"`（名字按同一规则）、`Bearer` / `Basic` 令牌、URL 的 `user:pass@`、`sk-...` 只换值本身；不截断。`image_data_url` / `scene_json` / `scene` / `content_base64` / `_artifacts` 等大块字段先脱敏再另存附件（data URL 解码成图片），超 256 KiB 的请求或响应体能解析成 JSON 的按字段脱敏后整份另存、否则原样另存（文字里有凭据片段时不存并写明原因）。`generate` / `generate_failed`（大块字段只留字节数，附件见同一调用的 `api_begin`）、`job_start` / `job_progress`（每过 1%，带状态报告）/ `job_end`、`feedback_jev`（类别与判断来源写中文）。写入队列不设上限，入队不阻塞急停 `0x18` 或异步音频协程；内存里待写附件超过 1 GiB（硬盘太慢）时新附件不收并写一步说明，记录本身照收。按目录名日期保留 30 天、不按大小删，启动时与记录中每 10 分钟或跨日时清理一次，清理不看开关。桌面会话启用时要求 `X-Moss-Desktop-Session`（独立 `moss-host` 与 Android 维持回环可写，局域网始终 403 `lan_read_only`）。先校验全部字段，再加锁原子读-改-写 `settings.json`（独立临时文件 + 改名，保留其他键，坏 JSON 拒绝覆盖）；没有运行目录或写入失败返回 500 `settings_write_failed`，不再显示“已保存”（2026-10-14）。`mcp_endpoint` 保留以兼容旧前端、独立 `moss-host` 与手机端；桌面端页面改在语音中心修改（技术书 §9.5.1），由语音中心直接写 `settings.json` 并立即重连。普通 Jev 配置写入运行目录 `settings.json`；Key 仍以原有方式存储于 SQLite `app_secrets`，本次不改变存储安全方案。旧 `jev_endpoint` 字段仅接受空值或固定 OpenRouter Decisions 地址，其他地址返回 400；已保存的旧地址不参与请求路由。`jev_delete_api_key:true` 删除已保存的 Key。`POST /api/system/check-update`（仅回环；桌面会话开启时要求 `X-Moss-Desktop-Session`）接受可选 `{include_prerelease}`，读取固定 GitHub Releases API（禁用重定向、6 秒超时），返回 `{current_version, latest_version, has_update, prerelease, published_at, release_url, notes, checked_at}`。

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
| `/v1/auth/challenge` | POST | 生成 6 位数字配对码，`result:{pairing_code, expires_at, valid_seconds}`，5 分钟过期；**仅回环地址允许**，启用桌面会话时须带有效 `X-Moss-Desktop-Session`（否则 403 `desktop_session_required`） | 回环（桌面版须桌面会话） |
| `/v1/auth/pair` | POST | `{pairing_code}` → `result:{token, pairing_id}` | 非回环时唯一免 Bearer 的数据接口 |
| `/v1/approvals?status=pending` | GET | 审批列表（新的在前，最多 50 条，先处理到期）：`result:{approvals:[…], server_time, ttl_seconds}`；每条为 `ApprovalRecord` 加 `target:{mode, label}`（串口显示端口名，网络设备显示“网络设备”，**不含地址**）；`status` 可为 `pending/approved/rejected/expired/executed/failed/all`，其他值 400 | 桌面会话 |
| `/v1/approvals/{id}/approve` | POST | `{digest}`：按当前记录重算摘要，与审批卡提交值和创建时的值都一致才批准，随后立即以审批身份重放原请求（使用审批保存的连接参数，走全部现有门禁）。成功 200 `result:{approval, result}`；失败透传执行错误码，`detail.approval` 为最新审批状态 | 桌面会话 |
| `/v1/approvals/{id}/reject` | POST | `{reason?}` → `result` 为拒绝后的审批 | 桌面会话 |

认证规则（在 §1.6 来源校验之后）：来源为回环地址时所有路由免认证。非回环时，`GET /api/health`、`/v1/capabilities`、`/api/system/info`、页面与静态资源（`/`、`/draw`、`/draw/*`、`/material-lab`、`/twin`、`/voice`、`/camera`、`/assets/*`、`/vendor/*`）公开，`POST /v1/auth/pair` 免 Bearer，其余数据接口（`/api/*`、`/v1/*`、`/preview`）要求 `Authorization: Bearer <token>`，缺失返回 401 `需要 Bearer 鉴权`，无效返回 401 `Bearer 鉴权失败`。

审批请求编号（2026-10-10）：可信语音 device/session、局域网 pairing_id 或网关 session 构成调用方作用域；新记录 `request_id` 为 `scope-v1:` 哈希。相同作用域和编号仅在执行参数及摘要一致时复用，不一致返回 409 `approval_request_conflict`；超过 128 字符拒绝。旧记录保持可读；旧 McpBridge 没有独立可信身份，不按自报编号复用审批。`voice_session_revoked` 表示旧语音会话不再有调用权，重新允许设备不恢复旧会话和旧审批。

桌面会话（技术书 §5.3，2026-10-06）：Tauri 桌面壳启动时调用 `ApiService::enable_desktop_session` 生成 32 字节随机会话值，页面经命令 `get_desktop_session` 取得、只存内存（该命令只交给 `main` 窗口且页面来源等于本进程后端地址，其他窗口或来源返回错误，2026-10-13），所有 API 请求带 `X-Moss-Desktop-Session`。该头**只在回环连接上有效**；值不对、或后端没有开启会话（独立 `moss-host serve`）、或来自非回环连接时一律 403 `desktop_session_invalid`（前端重新取会话后只重试一次）。有效时调用方上下文为 `Desktop`：严格模式下可直接发送，并且是唯一能调用 `/v1/approvals*` 的身份；其他身份调用审批接口得到 403 `desktop_session_required`。

审批相关错误码：`approval_not_found` 404、`approval_expired` 410、`approval_consumed` / `approval_rejected` / `approval_not_approved` / `approval_mismatch` 409、`approval_invalid_request` / `target_required` 400、`approval_unavailable` 503；严格模式下非桌面来源的 R3 请求得到 202 `result.status = "approval_required"`（`approval_id`、`expires_at`、`summary`、`speech`），回环无凭据得到 403 `strict_mode_blocked`。

语音中心（技术书 §9.4.6，P4-07）**不是 HTTP 接口**：以 Tauri 命令提供，只能由 MOSS 桌面窗口调用；改写设置、启停内置服务、测试/保存模型、本地模型下载删除、设备允许与改名，以及视觉/音乐/Agent 全部命令，另要求调用窗口为 `main` 且来源等于本进程后端地址，否则返回 `voice_acl_denied`（2026-10-13）；只读的 `voice_status`、`voice_mode_changes`、`voice_builtin_status` 不受此限；HTTP 后端（桌面内置、独立 `moss-host serve`、Android）都没有 `/v1/voice/*`，一律 404（A-68）。命令与参数：`voice_status()`、`voice_save_settings({settings})`（返回与 `voice_status` 相同的内容，另含 `reconnected`：本次保存是否重新发起了接入点连接且有桥启动；地址未变或没有可连接的接入点时为 `false`；握手是否成功看 `bridge.connected`，2026-10-14）、`voice_mode_changes({mode})`、`voice_set_mode({mode})`（`official` / `builtin`；其他值含旧的 `self_hosted` 返回 400 `voice_mode_invalid`；官方 ↔ 内置切换不改接入点与严格模式）、`exit_application()`。`voice_status` 返回 `mode`、`settings`（`builtin_ws_port`、`builtin_http_port`、`builtin_mqtt_*`、`builtin_udp_port`、`official_mcp_endpoint`（当前顶层 `mcp_endpoint`，脱敏）与 `official_mcp_endpoint_set`）、`mcp_endpoint`、`bridge`、`strict{enabled, from_env}`、`gateway`、`mcp`。2026-11-17 起自建服务端链路删除：`voice_server_start/stop/restart`、`voice_server_log`、`voice_models_test`、`voice_models_save`、`voice_detect_python` 不再存在，`voice_status` 不再含 `server`、`server_config`、`endpoint`、`device`、`models`、`strict.locked` 及自建相关 `settings` 字段，`voice_save_settings` 传入 `server_dir` / `python` / `args` / `server_port` / `self_hosted_mcp_endpoint` / `endpoint_health_url` 及其 `clear_*` 时忽略，`exit_application` 不再接受 `stopServer`。接入点与工具（NV8）：`voice_save_settings` 另接受 `add_extra_mcp_endpoint`（附加官方接入点，`ws://` / `wss://`、不含占位符、不能重复、最多 4 个）、`remove_extra_mcp_endpoint`（编号 `extra-N`）、`disabled_tools`（关闭的工具名数组，整体替换；未知工具或 `laser_safe_action_tool` 返回 400 `voice_settings_invalid`），写入 `settings.json` 顶层 `mcp_extra_endpoints` / `mcp_disabled_tools`（列表为空时删除键）并按需重连小智桥；`voice_status` 另含 `mcp`：`{endpoints[]: {id, endpoint(脱敏), connected, connected_at, connections, last_error, client, identity{fields, issued_at, expires_at, expired} | null}, max_extra_endpoints, tools[]: {name, description, enabled, can_disable, voice_hidden, calls, failures, denied, last_at, last_caller}}`。被关闭的工具经接入点或内置语音调用时返回工具错误 `tool_disabled`。内置语音服务（NV1）：`voice_builtin_status({lite?})`（`lite=true` 时省略数 MB 静态表情/画风 GIF 字典并启用 4 秒本地环境/IP 探测缓存供 2s 轮询复用，首屏与动作变更仍全量拉取，2026-11-12）、`voice_builtin_start()`、`voice_builtin_stop()`、`voice_device_set_status({deviceId, status})`（`allowed` / `revoked`）、`voice_device_rename({deviceId, name})`、`voice_device_remove({deviceId})`（n134：仅限 `allowed` / `pending` 设备，断开在线会话、同事务过期未消费语音审批、清理待推送固件/资源包与 `builtin_device_prefs` 并删除 `voice_devices` 行，保留对话记录与长期记忆；`revoked` 设备返回 409 `voice_device_revoked`）、`voice_builtin_models_test({section, input})`（`llm` 结论附分段耗时与思考提示，快速回答开启时带关闭思考参数测试，返回可含 `thinking_off`：`applied` / `unsupported`，n73；密钥留空时用当前分区或同源 OpenAI 兼容分区已保存的密钥；大模型使用流式 `stream: true` 探针，收到首个 `tool_calls` 分片即判定成功返回，同时兼容非流式 JSON；语音合成测试通过时另在 `audio_data_url` 返回合成音频 Data URL 供前端试听；识别、合成的 `input.kind` 可为 `doubao`——豆包流式，地址为 `wss://`、`model` 为资源 ID，二者留空用默认值，缺省 `openai`；合成的 `input.kind` 另可为 `dashscope`——千问 AI / 阿里云百炼原生 SSE 流式合成，支持将根域名或 `/compatible-mode/v1` 自动规范化至 `/api/v1/services/audio/tts/SpeechSynthesizer`，默认模型 `qwen-audio-3.1-tts-flash`，且 `section="tts_clone"` 支持上传本机 WAV/MP3/M4A 录音创建复刻音色并生成试听语音，n123；`section="local_tts"` 试听本地合成音色，`input` 为 `{model_id, speaker, speed?, transcript?}`，`speed` 须在 0.5～2.0，非 1.0 时当场合成、不读也不写本机试听缓存，Qwen3-TTS 0.6B 固定按 1.0、1.7B 把语速换成说话指令，`qwen3_tts_1_7b_clone` 按当前参考录音与可选 `transcript` 草稿当场克隆试听，返回另含 `speed`，n124 / n126 / n131；`section="local_tts_clone"` 管理本地离线声音克隆参考音色，`input` 为 `{action: "upload"|"update_transcript"|"clear", file_name?, audio_base64?, transcript?}`，校验 3～30s / ≥16 kHz / ≤7 MB WAV 并归一化为标准 16-bit 单声道 PCM WAV 落盘（单调递增 `generation`），`transcript` 留空上传时仅调用本机已安装的本地语音识别提取原话（绝不外发云端），脱敏返回 `tts_clone_ref{configured,generation,file_name,sample_rate,duration_secs,transcript,mode}` 与更新后的 `status`，n131）、`voice_builtin_models_list({section, input})`（共享 Keep-Alive 连接池，5 秒连接超时 / 8 秒列表超时，读取该服务商 `<基础地址>/models` 的全量模型 ID 列表 `models[]` 与原生类型分类映射 `model_sections`，不要求先填模型名与音色，密钥留空时沿用当前或同源分区已保存密钥，结果与报错不含密钥；`doubao` 与 `dashscope` 直接返回内置常用资源 / 模型列表，`section="tts_voices"` 读取千问 / 百炼账号已复刻音色列表；`llm` 指向 `api.cline.bot` 时置顶补齐 `cline-pass/*` 订阅模型并在 `/models` 返回 404 时回退列出订阅模型；OpenAI 兼容测试/对话/视觉对 `api.cline.bot` 附带 `X-Response-Format: openai` 与 Cline 客户端头并兼容解包 `{data:{choices}}`）、`voice_builtin_models_save({input, clear})`（`clear` 为要删除的分区；密钥留空时支持继承同源分区已存密钥；运行中会重启内置服务，返回 `restarted`）；本地模型（NV6 / n93）：`voice_local_save({settings})`（`llm_fast_reply`：快速回答，默认 `true`，对认识的服务商在大模型请求中附带关闭思考参数，服务不认时自动去掉重试，n73，2026-11-08；`asr` / `tts` / `vad` / `denoise` / `kws` / `turn` 开关、`vad_model` / `asr_model` / `tts_model` / `denoise_model` / `kws_model` / `turn_model`（选用的模型 id，须存在且类别相符，否则 400 `voice_settings_invalid`；`denoise`、`kws` 与 `turn` 默认 `false`）、`tts_speaker`（`qwen3_tts_0_6b` / `qwen3_tts_1_7b` 支持 0～8 音色并在切换模型越界时自动归位）、`tts_speed`、`mirror`；非热更新字段在运行中会重启内置服务，返回状态另含 `restarted` 与 `hot_applied`（是否全部为热更新字段、无需重启，含 `screen_card_style`、`debug_trace_enabled`、`chat_log_enabled`、`clear_debug_traces`、`persona_style`、`persona_prompt`、`custom_personas`、`persona_risk_notes`、`screen_pack_auto_keep`））、`voice_local_model_download({id})` / `voice_local_model_cancel({id})` / `voice_local_model_delete({id})`（`CATALOG` 30 项 id：判停 `silero_vad` / `silero_vad_v6` / `ten_vad`，识别 `sense_voice` / `sense_voice_2025` / `x_asr_streaming` / `qwen3_asr_0_6b` / `firered_asr2_ctc`，合成 `kokoro` / `kokoro_v11` / `matcha_zh_en` / `qwen3_tts_0_6b` / `qwen3_tts_1_7b` / `qwen3_tts_1_7b_clone` / `zipvoice_distill` / `moss_vits`，前级降噪 `gtcrn_simple` / `dpdfnet_baseline`，命令词抗噪 `kws_zipformer_zh`，尾音语义判停 `smart_turn_v3`，声纹 `campplus_sv_zh_cn_16k_common`，Jev `onejev_1_6_0_8b` / `onejev_1_6_4b` / `onejev_1_6_9b` / `laya_multilingual`，视觉 `qwen3_vl_2b` / `qwen3_vl_4b` / `qwen3_5_2b` / `qwen3_5_4b`，小智屏幕全量补字库 `xiaozhi_noto_v1_full`；删除判停/识别/合成/降噪/语义判停模型时等待 `chain_test_lock` 并拦截正在运行的相关推理子进程 `moss-voice-local`、`turn_worker`、`SpeechCppWorker`、`AudioCppWorker` 及兜底合成 `FallbackTtsDeleteGuard`，返回 409 `local_model_in_use`，而 `xiaozhi_noto_v1_full` 与视觉模型不受上述占用拦截、运行期可直接删除，删除本地合成模型前自动停止后台音色试听预热子进程释放句柄；下载/删除 `qwen3_tts_0_6b` / `qwen3_tts_1_7b` 按需管理（两档共用，删掉最后一档才删运行时） `<models>/.runtime/speech-v0.10.1/<platform>/` 免 Python 运行时，下载/删除 `qwen3_tts_1_7b_clone` 按需管理 `<models>/.runtime/audiocpp-v0.9.6/<platform>/` 免 Python 运行时（已上传的参考录音保留在 `<runtime_root>/local-tts-clone/`）；`xiaozhi_noto_v1_full` 在内置服务启动时后台按需自动拉取官方 `files.xiaozhi.me` 归档，用户手动取消或删除后写 `.noto_v1_full_dismissed` 标记不再自动重下，再次手动点击下载即清除标记，n83，2026-11-12；多个模型可同时下载，同一模型重复下载返回 409；删除先把模型整体改名到 `.trash-*` 再删，文件被占用时返回 500 `local_model_delete_failed` 且模型原样保留；删掉同目录最后一档 OneJev/视觉模型时一并删除自动下载的推理引擎 `<models>/.runtime/llama-*`（用户自备的 `<models>/llama` 不动）；删除 `laya_multilingual` 时取消进行中的环境准备，并删除带 `.moss-managed` 标记的 MOSS 自建虚拟环境，2026-11-07）、`voice_local_laya_setup({action})`（`start` / `cancel`：用本机 Python 3.10+ 在 Laya 根目录创建 `.venv` 并安装 `laya[serve]`，有 NVIDIA 驱动时先装对应 CUDA 版 PyTorch；本机没有可用 Python 或设置了 `MOSS_LAYA_PYTHON` 时返回 412 `laya_env_missing` 并说明原因；下载 `laya_multilingual` 且运行环境未就绪时后端自动开始准备，`laya_multilingual:force` 只下载权重，2026-11-07）；`voice_builtin_status` 另含 `sessions[]`（按每台在线设备的当前活跃会话去重，过滤重启残留的半开旧连接，每条另带 `text_font_size` / `text_font_bpp`，n83 / n84）与 `local`（设置、`models[]` 的 `id`、`kind`（`vad` / `asr` / `tts` / `denoise` / `kws` / `turn` / `speaker` / `jev` / `vision` / `font`）、`engine`、`label`、`note`（优缺点）、`size`（含附加文件）、`streaming`、`voices{default, max, hint}`（仅多音色合成模型）、`speed_adjustable`（仅合成模型：能否调语速，Qwen3-TTS 0.6B 为 `false`，前端不显示语速框，n126）、`installed` 与 `progress{state: downloading/preparing_runtime/waiting_runtime/verifying/installing/failed/cancelled}`（`preparing_runtime{done,total}`：OneJev 正在下载共用推理引擎；`waiting_runtime`：另一档正在安装推理引擎，排队中，可取消）、`speech_env{installed,ready,binary_found,accelerator_label,summary}`、`audiocpp_env{installed,ready,binary_found,backend,accelerator_label,summary}`、`tts_clone_ref{configured,generation,file_name,sample_rate,duration_secs,transcript,mode}`、`laya_env`（另含 `managed`、`venv_dir`）、`laya_setup{state: idle/running/done/failed/cancelled, stage, step, steps, detail, elapsed_secs, summary, accelerator, message, log[]}`、`jev_runtime{model, state: cloud/not_installed/idle/stopped/starting/ready/failed, accelerator, error, notice}`（所选本地 Jev 的实际运行情况：`failed` 时 `error` 为后台启动失败或推理进程意外退出的原因，可能附推理进程最后几行错误输出；`notice` 为 9B 自动关闭“没说完”判断、无显卡加速时 4B / 9B 偏慢等提示，2026-11-07）、`helper_found`、`helper_running`、`ready`）。`voice_builtin_status` 返回 `mode`（`chat` / `echo`）、`link_mode`、`ota_url`、`ota_ip_url`、`ota_ip_urls`、`models`（每个分区的 `configured`、`key_set`、`kind`、地址与模型，**不含密钥**）、`last_error`；错误码另有 `builtin_start_failed`（409，端口被占用等）、`voice_device_revoked`（409，已撤销设备不可移除）、`voice_device_not_found`（404）、`voice_device_invalid`（400）。内置服务对设备开放的端口只提供小智协议：`GET/POST /xiaozhi/ota/`（OTA 端口，默认 8003）与任意路径的 WebSocket（设备端口，默认 8000），见内置语音技术书 §13。失败统一为 `{code, message, status}`，常见 `code`：`voice_settings_invalid` / `voice_mode_invalid` / `model_input_invalid`（400）、`voice_mode_switch_failed`（已回滚）、`voice_desktop_only`（移动端）。返回内容中带令牌的地址均已脱敏，不含任何模型密钥。

配对语义（`AuthManager`）：配对码按来源 IP 限流，连续错误 5 次锁定 15 分钟，锁定时返回 429 `配对请求过于频繁，请稍后重试` 并带 `Retry-After`；配对码无效、已使用或已过期统一返回 400 `配对码无效或已过期`，不通过文案区分原因。token 为 32 字节 CSPRNG 随机值的 Base64，服务端只保存其 SHA-256 哈希与 `pairing_id`。已配对 token 哈希（`lan_paired_tokens`）与固定配对码哈希（`lan_static_pin_hash`）在 `AuthManager` 锁内同步持久化于运行目录 `settings.json`（重启后仍保持配对，支持通过 `/api/settings` 全部撤销，写盘失败中止变更；已删除的 `auth_pairings` 表不再写入），仅 5 分钟临时配对码与限流计数存在进程内存中。非回环部署默认仅限受信任局域网明文 HTTP，禁止公网直连；跨网必须由前置代理终结 TLS。移动端复用同一套路由，不新增独立的危险操作接口。

### 5.0 语音记录与诊断（n68，2026-11-07）

- **事件**：`voice_builtin_status.events[]` 为全部保留的事件，新的在前，每条 `{seq, at, at_ms, level, source, category, text, hint?, detail?}`：`seq` 递增序号；`source`（n76，2026-11-09）为 `xiaozhi`（小智设备、对话与模型链路）/ `laser`（激光雕刻机：握手、走边框、急停、设备操作失败、加工完成 / 停止 / 失败，经 `laser_api::ApiService::set_event_sink` 写入）/ `moss`（MOSS 软件本身：`internal` 分类、内置服务启停与启动失败、本地视觉模型进程、应用内更新失败），定义在 `moss_voice_server::DiagSource`，缺省视为 `xiaozhi`；`at` 为 Unix 秒（兼容旧界面）、`at_ms` 为 Unix 毫秒；`level` 为 `info` / `warn` / `error`；`category` 为闭集 `general`（一般流程）/ `network`（网络）/ `auth`（密钥或权限）/ `quota`（余额、额度、频率）/ `config`（地址、模型名、参数）/ `device`（小智设备）/ `firmware`（固件与资源包）/ `model`（模型服务或本地模型程序）/ `audio`（解码、判停、播放）/ `internal`（MOSS 内部），定义在 `moss_voice_providers::DiagCategory`，前端大白话映射在 `voiceState.js` 的 `VOICE_DIAG_CATEGORIES`；`text` 是大白话说明，`hint` 是下一步建议，`detail` 是原始技术信息（底层错误链、服务方原话），写入前经 `scrub_diagnostic_text` 去掉疑似密钥、`token=` 一类参数、`Bearer` 后的值、地址里的账号密码与查询串、IPv4 地址，最长 2000 字并标“已截断”。没有建议或细节时不输出这两个字段。
- **保留与覆盖**：事件与耗时存在桌面端 `BuiltinVoice` 持有的 `DiagnosticsLog`（只在内存，不落盘），作为 `ServerConfig.diagnostics` 传给每次启动的服务，**服务停止、重启、崩溃重启都不清空**，退出 MOSS 才清空。日常记录（`info`）最多 300 条，错误与警告另一个缓冲最多 200 条，日常记录挤不掉错误；耗时最多 50 轮。`events_dropped: {info, problems, timings, max_info, max_problems, max_timings}` 为已被覆盖的条数与各自上限，界面据此提示，不悄悄丢。排障原文 `debug_traces` 自 n81（2026-11-12）起默认开启（`builtin_local.chat_log_enabled=true`）并脱敏落盘到 SQLite `voice_chat_logs`（上限 500 条，状态接口读最近 200 条，对外导出诊断包在关闭开关时仍为空、开启时限最近 20 条），每条含完整回复 `reply`、屏幕已播句子 `spoken`、`truncated`（文字超过 2000 字或工具调用超过 16 个）与 `tools_dropped`，关闭开关只暂停新记录、清空须显式确认。
- **状态新增字段**：`events_dropped`；`warnings[]`（当前生效的降级与迁移提示：本地音乐目录不可用、声纹按人记忆未生效、Windows 系统重置后 `Windows.old` 中仍有未迁回的旧模型或旧语音配置）；`timings[]` 每轮另有 `device_id` 与 `at_ms`（结束时间）；`outcome` 取值 `running` / `done` / `interrupted` / `ignored`（没识别出文字）/ `music_started` / `disconnected`。服务未运行时 `events` / `timings` 仍返回记录本内容。
- **写进记录的桌面端后台失败与提示**（以前只打印到看不见的控制台）：本机 Agent 只读工具挂载失败、本地模型下载残留清理、结束上次遗留的 OneJev 进程、固件下载记录恢复失败、本地 Jev（OneJev / Laya）各处启动失败、内置服务启动或重启失败（含随 MOSS 自动启动）、本地音乐目录不可用、声纹按人记忆未生效、Windows 系统重置后 `Windows.old` 旧版配置或模型待迁回提醒。
- **提供方错误**：`ProviderError {message, category, hint, detail}`；网络错误按底层原因分开说（超时 / 域名解析失败 / TLS 证书 / 对方拒绝连接 / 网络不通 / 中途断开 / 其他）；HTTP 状态按 401、403 → `auth`，402、429 → `quota`，400、404、422、3xx → `config`，5xx → `model` 归类，服务方原话放 `detail`。对话出错记为 `error`；已自动补救的（流式识别失败改整段识别、主合成失败降级本地合成）记为 `warn`。设备异常断开说明最可能原因（Wi-Fi 掉线、设备重启或断电）。
- **命令错误**：`CenterError` 另有可选 `detail`（原始技术信息，已脱敏）；数据库失败统一为“MOSS 本机设置数据库读写失败……”，Debug 文本只进 `detail`。
- **审批列表**：`GET /v1/approvals` 结果另有 `limit`（50）。界面写“最近 N 条”，满额时注明只显示最近 50 条。
- **导出诊断包**：`voice_diagnostics_export()`（无参数；与 `voice_builtin_status` 同权限：`default` capability 授权，命令开头 `require_status_window` 固定为主窗口）。后端生成 JSON 后弹出系统“另存为”框，路径与内容都不来自网页；取消返回 `{saved:false}`，成功返回 `{saved:true, file_name, bytes, events}`（不回传完整路径），写入失败返回 `diagnostics_export_failed`。文件内容：`{format:"moss-voice-diagnostics", format_version:1, generated_at_ms, app{name, version, os, arch}, link_mode, running, mode, ports{ws, http}, models, jev, local{ready, helper_found, helper_running, models, jev_runtime, speech_env, onejev_env, laya_env, laya_setup, asr_model, tts_model, vad_model, denoise, denoise_model, turn, turn_model}, last_error, warnings, events, events_dropped, timings, debug_enabled, debug_traces, sessions, devices[{device_id, name, model, firmware_version, status, last_seen}], approvals{limit:50, items, error}, included[], excluded[]}`。
- **Colab T4 专属联动、Colab CLI 自动部署与笔记本导出（n80，2026-11-10）**：`voice_builtin_status.local.colab` 返回 `{enabled, url, llm_model, tts_model, tts_voice, tts_speed, key_configured, active, cli:{installed, logged_in, account_hint, expires_in, auth_url, runner_state, runner_step, runner_error, ref_audio_configured, ref_audio_name}}`（密钥只存 `app_secrets` 的 `voice.colab.main`，永不回传；账号邮箱由 `mask_email` 脱敏）；`voice_local_save` 接受可选 `colab: {enabled?, url?, llm_model?, tts_model?, tts_voice?, tts_speed?, api_key?, clear_key?}` 与 `colab_cli: {action: "refresh"|"install"|"login_start"|"login_complete"|"logout"|"start_session"|"stop_session"|"set_ref_audio"|"clear_ref_audio", code?, file_name?, wav_base64?}`（已登录 `colab` CLI 时开启 `colab.enabled=true` 且地址或密钥为空会自动在后台启动 `colab` CLI 分配云端 T4、同步本机 `<runtime_root>/colab-cli/moss_ref.wav` 550W 参考音频并回填隧道地址与密钥）；`voice_builtin_models_test("colab", {target?: "all"|"llm"|"tts", ...})` 探测 Colab 隧道；`voice_colab_export_notebook()`（主窗口 `require_main_window`）由后端生成固定 `.ipynb` 并弹出系统保存框，返回 `{saved, file_name?, bytes?}`。
  - **包含**：版本与系统、链路与端口、模型服务类型 / 地址 / 模型名 / 音色与“是否填过密钥”、本地模型与 Jev 状态、全部保留的事件与覆盖计数、耗时（含会话号与设备编号）、对话记录开关（`chat_log_enabled`，默认 true）打开时的最近 20 轮脱敏对话记录（关闭时为空）、在线会话与已登记设备、最近 50 条审批（含结果说明与错误码）、最近一次启动失败与降级提示。
  - **不包含**：任何密钥 / 令牌 / 密码（只记录是否填过）、录音与合成音频、对话记录开关关闭时的对话文字、对话记忆 / 长期记忆 / 资料库 / 声纹、家居地址与令牌、固件文件、设备上报的原始状态快照。
  - **脱敏**：整个 JSON 的每个字符串再经 `moss_voice::redact::redact_line` 与 `scrub_diagnostic_text`；IPv4 一律换成 `<ip>`（四段数字的版本号也会被换掉），域名与设备编号保留，便于排障。

### 5.1 第三方 MCP（NV9，2026-10-10）

桌面新增 `voice_mcp_status`、`voice_mcp_save({input})`、`voice_mcp_delete({name,revision,confirmed})`、`voice_mcp_reconnect({name,confirmed})`。只授予主窗口且在命令入口再次核对实际回环来源与桌面会话，没有 HTTP 配置／执行接口。输入、返回、凭据引用与失败语义见 [第三方 MCP 契约](../integration/moss-external-mcp.md)。

现有审批列表增加 `kind=external_tool`、`target.mode=external` 和 `external:{tool,arguments}`（仅有界脱敏预览）。批准仍只接受桌面身份，使用完整隐藏 request 和当前第三方连接／权限重算摘要；授权只消费一次，不能使用参数中的 `confirmed` 或 `approval_id` 绕过。外部审批与激光审批使用各自的执行分支，不会把外部参数变成设备命令。第三方结果不确定或结果存储失败不自动重试；连接恢复也不补发旧调用。

### 5.2 应用内更新命令（2026-11-07）

客户端新增 Tauri 命令 `app_update_status()` 与 `app_update_install({tag})`（无 HTTP 接口），入口校验受信任主窗口（桌面端 `window_is_trusted_main`，Android 端 `is_trusted_mobile_main`）。`app_update_status` 返回 `{supported, unsupported_reason, platform, phase, version, downloaded, total, error}`，其中 `platform` 为 `windows` / `macos` / `android` / `other`，`phase` 为 `idle` / `checking` / `downloading` / `installing` / `restarting`（桌面）/ `installer_launched`（Android）/ `error`。`app_update_install` 在 `ApiService::device_work_in_progress()` 为真时拒绝（下载完安装前二次复核），只从固定前缀 `https://github.com/yjdaaa/moss/releases/download/<tag>/latest.json` 拉取更新包并用内置公钥校验 minisign 签名与 `version:` 字段；桌面端按“完全退出”顺序急停并停服务后替换重启，Android 端把校验通过的 APK 暂存于应用私有缓存并经 `FileProvider` 拉起系统安装器确认升级。

### 5.3 本机 AI 调试入口、测试时间窗与数字音频对照（VA / VC，2026-11-27）

位于语音侧 `moss-voice-server::debug_api`，独立绑定 `127.0.0.1`（默认端口 `18009`，被占用时自动分配并写入 `<runtime>/debug/voice-debug.json`，绝不挂在 `laser-api` 或 `0.0.0.0:8003` 上）。仅在桌面主窗口开启 `builtin_local.debug_text_inject_enabled` 后监听；开启时在 `<runtime>/debug/voice-debug.token`（权限 `0600`）写入 32 字节随机令牌，关闭开关或开启满 24 小时后自动作废删档（同时自动关闭全部测试时间窗）。所有路由要求：① 对端为回环地址（否则 403 `loopback_only`）；② 不携带 `Origin` 请求头（否则 403 `origin_not_allowed`）；③ 开关开启且未过 24 小时有效期（关闭返回 403 `debug_disabled`，过期返回 401 `token_expired` 并自动清理文件、关闭开关与时间窗）；④ `Authorization: Bearer <token>` 常量时间匹配（缺失或错误返回 401 `unauthorized`）。

| 路径 | 方法 | 请求体 / 参数 | 响应与错误 |
|---|---|---|---|
| `/debug/v1/devices` | GET | 无 | 200 `{mode, active_laser_job, bench_windows:[{device_id, session_id, opened_at_ms, expires_at_ms, max_minutes}], devices:[{device_id, session_id, state, level, recent_peak_level, latest_turn, latest_turn_id, bench_window, transport, connected_at, device_mcp_enabled, device_tools_ready, device_tools_count, screen_monochrome}]}` |
| `/debug/v1/devices/{device_id}/turns` | GET | 查询参数 `limit?`（默认 20，限 1..=50） | 200 `{device_id, turns:[TurnDetail...]}`，按轮次倒序返回该设备近期完整明细 |
| `/debug/v1/bench-window` | POST | `{device_id, action:"open"\|"close", max_minutes?}`（或布尔 `open`；`max_minutes` 默认 30、限 1..=60） | 打开或关闭该设备的整体测试时间窗：开启返回 200 `{device_id, action:"open", bench_window:{...}}`（离线 404 `device_offline`、撤销 403 `device_revoked`、回声模式 409 `echo_mode`、非法时长 400 `invalid_max_minutes`）；关闭返回 200 `{device_id, action:"close", closed, bench_window:null}`。时间窗内该设备全部真实音频轮次强制按 `simulated=true` 处理，到期/断连/关开关/令牌过期自动关闭 |
| `/debug/v1/devices/{device_id}/inject` | POST | `{text: string}` | 强制以 `simulated = true` 注入到该在线会话并先下发 `stt` 字幕；成功 202 `{turn_id, device_id, session_id, turn, simulated:true, accepted_at_ms}`；空文本或超长 400 `text_invalid`，离线 404 `device_offline`，已撤销 403 `device_revoked`，回声模式 409 `echo_mode`，正在对话/播放/拾音 409 `device_busy`（绝不打断进行中对话） |
| `/debug/v1/devices/{device_id}/inject-audio` | POST | `{pcm_base64, sample_rate?:16000\|24000, frame_ms?:20\|40\|60, lead_silence_ms?, tail_silence_ms?}` | 仅在目标设备**已打开测试时间窗**时允许（未开窗 403 `bench_window_required`）；按设备帧节奏把 16-bit LE 单声道 PCM（最长 30 秒）注入该会话用于数字对照，触发轮次恒 `simulated=true`、`digital_audio=true` 且绝不进入 `asr-corpus`；成功 202 `{device_id, session_id, accepted_at_ms, duration_ms, frame_ms, frame_count, expected_min_turn}`；参数错 400、离线 404、回声/忙 409 |
| `/debug/v1/turns/{turn_id}` | GET | 路径参数 `turn_id` | 200 `{turn_id, device_id, session_id, turn, simulated, bench_window, simulated_label?, digital_audio, injected_text, corrected_text, route, outcome, started_at_ms, finished_at_ms, speech_began_at_ms?, speech_end_at_ms?, first_audio_frame_at_ms?, peak_uplink_level, corpus_date?, corpus_turn_id?, stages:{...}, tools:[{name, arguments, result, ms, blocked, block_reason}], reply, spoken, playback:{frames, underruns, stall_ms}, stop_skipped_reason?, device_online, device_state, device_level}`；不存在 404 `turn_not_found` |
| `/debug/v1/process-trace` | POST | `{enabled: bool, flush?: bool}` | 200 `{enabled, previous_enabled, trace_dir_rel, using_custom_dir}`；未挂载 503 `process_trace_unavailable`，读写失败 500 `process_trace_failed` |

不提供任何直接调用工具、修改其他设置或下发设备命令的接口。配套桌面 Tauri 命令沿用主窗口来源校验（`require_main_window`）：`voice_local_save({debug_text_inject_enabled})` 热启停上述入口（不断开在线小智，关闭时同步关闭全部测试时间窗）；`voice_device_request({deviceId, action:"inject_text", payload:{text}})` 仅在调试开关开启且未过期时走同一模拟注入链路（关闭时返回 403 `debug_inject_disabled`）；`voice_device_request({deviceId, action:"close_bench_window"})` 供桌面“立即结束测试”按钮主动关闭该设备的测试时间窗；`voice_builtin_status` 返回无密钥字段 `debug_inject{enabled, active, host, port, issued_at, expires_at, ttl_hours, token_file_rel, meta_file_rel, bench_windows[]}`，在线会话对象附带 `bench_window`、`latest_turn`、`recent_peak_level`，`debug_traces[]` 每条附带 `simulated: bool` 与 `bench_window: bool`（工具摘要附带 `blocked` / `block_reason`）。

### 5.4 识别（ASR）真实录音语料采集与标注命令（VB，2026-11-27）

无 HTTP 接口；仅通过敏感 Tauri 命令 `voice_asr_corpus_manage({input})` 提供，只授权给 `capabilities/voice-private-main.json` 的桌面主窗口，入口校验 `require_private_settings_window(&window)?`（不在 `default.json`）。`input.action` 取值：

- `"status"` / `"list"`：返回 `{enabled, max_items, max_days, corpus_dir_rel:"asr-corpus", valid_tags:["命令词","数字","材料名","有噪声"], total_count, labeled_count, items[]}`（关闭时 `items` 恒为空，不回传本机绝对路径）；
- `"set_config"`：`{enabled, max_items?, max_days?, confirmed?, delete_existing?}`（首次开启缺少 `confirmed: true` 返回 400 `confirmation_required`；关闭时 `delete_existing: true` 同步清空已采集语料；上限范围 1..=5000 条、1..=90 天，热生效不重启服务）；
- `"audio"`：`{date, turn_id}`（关闭态返回 403 `asr_corpus_disabled`；仅读 16 kHz 单声道 WAV 并返回 `{audio_data_url, duration_ms, sample_rate:16000}`）；
- `"save_label"`：`{date, turn_id, reference_text, verified_correct?, tags?}`（关闭态返回 403 `asr_corpus_disabled`；校验非空与合法标签后原子写入同目录 `<turn-id>.label.json`）；
- `"delete"`：`{date, turn_id}`（关闭态仍允许删除单条语料）；`"clear"`：`{confirmed: true}`（清空全部语料）。

`voice_builtin_status` 附带无录音、无文本的计数摘要 `asr_corpus{enabled, max_items, max_days, corpus_dir_rel, total_count, labeled_count}`（仅整数统计）；`diagnostics_bundle()` 严禁包含任何语料内容并在 `excluded` 显式列出“识别测评录音语料与标注”。

### 5.5 整体测试真人录音语料管理命令（VD2，2026-11-27）

无 HTTP 接口；仅通过敏感 Tauri 命令 `voice_bench_recordings_manage({input})` 提供，只授权给 `capabilities/voice-private-main.json` 的桌面主窗口，入口校验 `require_private_settings_window(&window)?`（不在 `default.json`）。`input.action` 取值：

- `"status"` / `"list"`：返回 `{recordings_dir_rel:"voice-bench/recordings", corpus_name, corpus_count, corpus_items[], total_recordings, speaker_count, complete_speaker_count, complete_speakers[], speakers[], recordings[]}`（`corpus_items` 来自内置 `scripts/voice-bench/e2e-corpus.json`，不回传本机绝对路径）；
- `"save"`：`{speaker, corpus_id, text?, audio_wav_base64}`（校验说话人标签与语料编号无路径穿越，解码并线性重采样至 16 kHz 单声道 WAV，校验时长 0.5–15 秒、电平不过低、无削顶，不通过返回 400 `bench_recording_quality_failed`；原子写入 `<runtime>/voice-bench/recordings/<speaker>/<corpus-id>.{wav,json}`）；
- `"audio"`：`{speaker, corpus_id}`（返回 `{speaker, corpus_id, sample_rate:16000, duration_ms, audio_data_url}`）；
- `"delete"`：`{speaker, corpus_id}`（删除单条录音与元数据）；`"delete_speaker"`：`{speaker, confirmed: true}`；`"clear"`：`{confirmed: true}`（清空全部测试录音，缺少 `confirmed: true` 返回 400 `confirmation_required`）。

`voice_builtin_status` 附带无录音、无绝对路径的摘要 `bench_recordings{recordings_dir_rel, corpus_count, total_recordings, speaker_count, complete_speaker_count, speakers[]}`；`diagnostics_bundle()` 严禁包含任何测试录音或说话人元数据，并在 `excluded` 显式列出“本机整体测试真人录音与元数据”。

## 6. 黄金样本生成要求（阶段 1）

黄金样本采集自 Python 版（`tests/golden/api/`），Rust 版不再自动差分，保留作为字段形态与错误文案的参考。掩码字段：`workflow_id`、`task_id`、`calibration_id`、`job_id`、`created_at`、`updated_at`、所有绝对路径、`content_fingerprint`、`source_path`、`backup_path`、`params_file`（Python 返回绝对路径，Rust 只返回文件名，值本身不可比）。注意样本文件本身仍含采集机器的绝对路径。材料库样本比对时，Python 侧 `null`/`{}` 墓碑节点视为该格不存在（db-schema.md §3.6.2）。比对时 `result.web.*_url` 只比路径部分。

比对方法（归档规格 §20.1 级别 1）：响应体解析为 JSON 后 deep-equal，不比文本。HTTP 状态码、`success` 值、`result`/`detail` 结构必须相等；错误文案字符串逐字相等，因为用户会看到；数值按数值比较；键序与空白忽略。`/preview` 返回的文件按 SHA-256 比较。黄金样本保存原始响应，掩码与规范化在比对时进行。
