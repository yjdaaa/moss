# MOSS 内置语音 NV10～NV19 实施细则

状态：实施方案 i4（2026-10-13，NV19 按 OpenViking 官方文档修正）。设计步骤不代表功能或真实验收已完成；进度只在 [实施 TODO](moss-fusion-implementation-todo.md) 维护，功能范围与已确认决定见 [内置语音技术书](moss-native-voice-spec.md)。

## 0. 开发入口与通用约束

1. 按原 NV10 → NV11 → NV12 → NV13 → NV14 → NV15 → NV16 → NV17 → NV18 顺序推进；NV19 列为后续阶段，先处理既有安全与交付阻塞，不重置旧任务。真实账号、模型、设备验收可以后补，但未测项目保留 `partial/unverified`，不能把软件记录勾成整体完成。
2. 每项使用六个栏目：技术框架、修改位置、实现步骤、数据与接口、异常与安全、验证标准。下文“拟新增”文件、类型、命令及测试名是实施契约；“已有”仅指本次在工作区核实的代码。
3. 配置扩展 `crates/moss-voice/src/settings.rs::VoiceSettings`，通过已有桌面命令边界保存；密钥仅进 `RuntimeDb::app_secrets`，复用 Keep/Set/Clear 语义。旧表单不得覆盖 NV9 的 `mcp_servers`。管理命令校验主窗口和实际后端来源，不新增局域网管理 API。
4. 新内置工具由 `ToolExecutor` 组合进入内置语音，名称不用 `__`（宿主把该模式分流到第三方 MCP）。复用 `HostTools` 回填和截断规则；第三方 MCP 保留现有 `ExternalMcp`，不将内置 R0 插件接进它的 Ask 审批路径。每次调用复核当前 voice permit、开关及参数；外部返回内容不能授予任何权限。
5. 新数据库迁移加入 `RuntimeDb::migrate`，按顺序、在事务中执行并可重跑。NV10-1 已将 `SCHEMA_VERSION` 迁移至 v7 并加上 `voice_memory`；NV13/NV16/NV17 后续使用实际下一可用版本，不覆写 v7。NV13/NV16/NV17 依次分配实际版本，不预留冲突编号。

软件交付共用门禁：更新 `CHANGELOG.md` 和受影响的 API、数据库、功能字典、提示词；运行 Rust 全量、前端单测和适用的回环 E2E，重新构建前端、Windows release、Android release APK。涉及 helper 时另运行其独立工作区测试与 release 构建。具体命令及权限见 [AGENTS.md](../../AGENTS.md)；根工作区测试不会覆盖独立 helper。硬件、真实账号、摄像头、安装、刷机和发布分别按其授权验收，不从开发请求推定授权。

## 1. 当前接线位置

| 能力 | 已有位置 | 后续接法 |
| --- | --- | --- |
| 会话、历史、播放 | `moss-voice-server/src/{session,conversation}.rs`、`audio/` | 会话负责轮次、取消、历史；插件不能直接向 WebSocket 写音频 |
| 工具适配 | `moss-voice-server/src/tools.rs::HostTools`、`ToolExecutor` | 增加内置工具组合，未匹配的名称委托现有 HostTools |
| 设备 HTTP | `moss-voice-server/src/ota.rs::router` | 8003 上增加视觉、固件下载；保留 OTA 原接口 |
| 桌面管理 | `apps/desktop/src-tauri/src/voice_builtin.rs`、`frontend/src/pages/VoicePage.jsx` | 沿用桌面命令、状态、错误呈现；新增卡片按功能拆文件 |
| 可信语音来源 | `laser-api/src/voice_authority.rs`、`laser-host/src/external_mcp.rs` | 复用会话失效、批准前重算、单次消费和审计规则；新增审批种类需要专用执行入口 |

<a id="nv10"></a>

## 2. NV10 记忆

### 技术框架

SQLite 保存文字摘要；复用当前 `Llm` 提供方生成总结，通过会话 system prompt 注入。总结是后台任务，不占用播放或设备消息循环。新增记忆开关默认关闭，用户开启后才总结和注入；NV16 之前按设备区分。

**NV19 衔接**：保留 NV10 原设备摘要、CAS 和开关，后续分阶段增加可检索条目而非替换旧库（[NV19](#nv19)）。

### 修改位置

- 拟新增 `crates/moss-voice-server/src/memory.rs`：总结队列、摘要注入、版本复核。
- 修改 `session.rs`、`conversation.rs`、`lib.rs`：在正常会话结束/断开处收集已提交历史，装配服务；记录会话只总结一次。server 不反向依赖 laser-runtime，拟定义 `VoiceMemoryStore`，以 `ServerConfig.memory: Option<Arc<dyn VoiceMemoryStore>>` 注入。
- 修改 `crates/laser-runtime/src/db.rs` 与迁移测试：记忆表和条件更新；修改 `VoiceSettings`。
- 修改 `apps/desktop/src-tauri/src/{voice_builtin,lib}.rs` 及对应命令权限：DbRegistry 实现 VoiceMemoryStore，桌面持有与 server 同一 MemoryService，列表/编辑/清空经其协调入口；拟新增 `frontend/src/components/VoiceMemoryCard.jsx`，接入 VoicePage。

### 实现步骤

1. 先做迁移和 DB CRUD。设备首次开启记忆时建空行；编辑、清空均递增 revision，后续异步任务只能条件更新已有行。
2. 加载摘要时检查开关、设备允许状态；以独立标注的“历史摘要数据”注入固定提示，不把摘要拼成工具规则。
3. 结束会话后从已提交 `history` 取最多 6000 字符，与旧摘要一起交给当前 LLM；忽略未开始播放的尾句和工具内部隐藏请求。空会话不总结。
4. 总结前捕获设备、旧 revision、记忆开关 generation 和会话许可；任务结束时全部复核，再以 revision 条件写入。每设备串行，全局最多 2 个总结任务、待处理最多 16 项；满队列跳过并记不含正文的事件。
5. 接入查看、编辑、清空和关闭。清空写空串并增加 revision，禁止旧总结把内容写回来；关闭取消待处理项。撤销设备阻止写回；重新允许不恢复旧任务。

### 数据与接口

拟新增表，`updated_at` 沿用 RuntimeDb 既有 `REAL` Unix 秒编码，桌面 DTO 同样返回数值时间：

```sql
CREATE TABLE voice_memory (
    device_id TEXT PRIMARY KEY NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    revision INTEGER NOT NULL DEFAULT 0,
    updated_at REAL NOT NULL
);
```

后台写入使用 `UPDATE ... SET summary=?, revision=revision+1, updated_at=? WHERE device_id=? AND revision=?`，影响 0 行表示结果过期，不能改用无条件 upsert。VoiceMemoryStore 定义异步 load/ensure/conditional_save 和桌面 CRUD 所需方法，宿主在 blocking worker 中访问 SQLite；MemoryService 统一管理开关 generation、每设备串行和取消，网络 await 不持有 DB 锁。编辑接口采用 `{device_id, expected_revision, summary}`，最长 1500 Unicode 字符；返回 `{device_id, summary, revision, updated_at}`。拟新增桌面命令 `voice_memory_list`、`voice_memory_save`、`voice_memory_clear`；revision 冲突返回 `memory_conflict`，前端保留用户输入并提示刷新。开关放 `voice.builtin_memory_enabled`，旧配置缺字段按关闭处理。

### 异常与安全

LLM 总结限时 30 秒；错误、空结果、超长结果或数据库失败均保留旧摘要，不保存完整对话作为补偿，不自动无限重试；下次新的会话结束可重新总结。关闭/清空/编辑与条件写回在同一协调锁内复核 generation，避免检查后才被清空的竞态。摘要不包含密钥、审批记录、连接地址或原始音频；模型产生的“已批准”等文字不改变权限。备份包含摘要，导出前按用户选择，不自动外传。

### 验证标准

- 迁移：从 v6 空库和已有设备库升级、重复迁移、失败回滚重试；旧设备和 `app_secrets` 保持原样。
- fake LLM：合并旧摘要、只注入对应设备、关闭不调用、失败保留旧值；总结耗时不阻塞 ping、播放和撤销。
- 竞态：总结中编辑/清空/关闭/撤销、两个会话连续结束；旧结果不能覆盖新版本或恢复清空内容。
- 前端：查看、1500 字符边界、冲突提示、清空及开关往返；软件门禁通过后才勾软件步骤。真实模型摘要质量另记 `unverified`。

<a id="nv11"></a>

## 3. NV11 信息插件

### 技术框架

本期先在 `moss-voice-server` 内增加内置工具模块，复用 `reqwest`、`serde` 与已有 ToolExecutor，不为五个插件建立新的独立 crate。农历候选用 `lunar_rust`，RSS 用受限 XML/RSS 解析器；实施时锁定版本、许可证及日期范围。[农历库源码](https://github.com/flameoftheforest/lunar_rust/blob/main/README.md) 提供公历/农历、节气与宜忌能力；不能将宜忌当成设备安全判断。

### 修改位置

- 已有 `crates/moss-voice-server/src/plugins/{mod,http,cache,runtime,weather,news,newsnow,lunar,search,metaso,rag}.rs`，复用既有工具组合。
- 已有 `VoiceSettings`、`voice_builtin.rs` 和 `frontend/src/components/VoicePluginsCard.jsx`；插件凭据独立存储，仅 packaged `voice-private` 管理。
- 合成供应商样本与固定依据见 [NV11 fixtures](../../crates/moss-voice-server/tests/fixtures/nv11/README.md)，真实本机TLS契约测试在 `plugins/contract_tests.rs`；前端实际React假桥在 `frontend/tests/voice-plugins.browser.mjs`。

### 实现步骤

1. 先实现共用有界 HTTP 客户端及工具组合；给每个提供方固定请求结构和成功/失败 fixture，再写服务映射。
2. 天气与农历先接线；天气设置保存 API host、鉴权类型和默认城市坐标。农历传入明确日期和配置时区，不让系统 UTC 跨日改变回答。
3. RSS、NewsNow、Tavily和独立Metaso适配已实现；Metaso按固定官方SDK普通搜索契约，不把专题API必填ID强加给普通搜索，不自动切换提供方或共享密钥。
4. RAGFlow 保存桌面选定的数据集白名单；工具不能用模型自填的数据集替换它。结果带来源标识后回填 LLM。
5. 每个插件默认关闭；配置完整且用户开启才进入列表。关闭、缺 key、无默认城市或不可用来源在调用处再次拒绝，旧工具列表不能绕过。

### 数据与接口

共同工具响应为 `{ok, source, observed_at, data, truncated}`；失败为 `{ok:false, error:{code, message, retryable}}`，不含原始响应或凭据。每项结果最多 2000 字符；标题、URL、时间取提供方原值，不让模型补造。

| 工具 / 模块 | 输入和实际请求 | 响应取值与本地限制 |
| --- | --- | --- |
| `get_weather` / weather | `{}`；只使用桌面保存的坐标，拒绝模型传city/host；和风 `GET /weather/v1/current/{latitude}/{longitude}`，host 由用户配置；`X-QW-Api-Key` 或 JWT Bearer 按配置选择 | 映射 temperature.value/unit、condition.text/code、humidity（0..1）、wind.speed.value/unit、metadata.attributions；保留来源声明。observed_at 为本地取数时间，不冒充接口未返回的观测时间；不套旧 `/v7/weather/now` 字段 |
| `get_news` / news | `{source:string, limit?:integer}`，limit 1..5；RSS `GET` 固定已配置源；NewsNow `GET /api/s?id=<白名单 source_id>` | RSS 读取 item 的 title/link/pubDate/description；NewsNow 适配 items/status/更新时间，禁止把接口失败等同“没有新闻”；缺摘要只读标题 |
| `get_lunar` / lunar | `{date?:"YYYY-MM-DD"}`，缺省取配置时区当天，初期界面范围 1900..2100，超范围明确拒绝 | `{solar_date,lunar_date,leap_month,solar_term,yi,ji}`；不可计算的字段省略并说明，不写虚构值 |
| `web_search` / search | `{query:string}`，1..500 字符；Tavily `POST /search`，Bearer key，body `{query,search_depth:"basic",max_results:5,include_raw_content:false}` | 取 results 中 title/url/content，按总长截断；不自动抓取命中网页、图片或附件 |
| `web_search` / metaso | `{query:string}`，1..500非空字符；按配置供应商选择Metaso，独立`voice.plugin.metaso`；`POST /api/open/search/v2`，Bearer，`{question,lang:"zh",stream:false,enableMix:false,newEngine:false}` | 官方SDK普通搜索，未带searchTopicId/sessionId；errCode=0、data.text/references；只回公开URL引用，不暴露上传文件/余额/会话元数据；不抓结果、不退回Tavily |
| `search_rag` / rag | `{question:string}`，1..500 字符；RAGFlow `POST /api/v1/retrieval`，Bearer key，body 含配置 dataset_ids、question、page=1、page_size=5 | 仅 code=0 为成功；取 data.chunks 的 content 与非空document_id/dataset_id；响应dataset_id也须在配置白名单；总长≤2000字符，旧retcode/缺来源/字段不符报schema错误 |

天气接口和鉴权采用 [和风当前天气](https://dev.qweather.com/en/docs/api/weather/weather-current/)、[鉴权说明](https://dev.qweather.com/en/docs/configuration/authentication/)；搜索与检索采用 [Tavily Search](https://docs.tavily.com/documentation/api-reference/endpoint/search)、[RAGFlow HTTP API](https://ragflow.io/docs/http_api_reference)。NewsNow 需在 NV11-1 固定实际服务版本与公开源码 fixture；中新网 RSS 的实际可达源也在该步骤核实，未核实的示例域名/源不得作为默认可用服务。

非敏感配置放 `voice.builtin_plugins`，插件 key 为 `voice.plugin.weather/search/rag` 等命名空间；JWT 私钥如需支持也是 secret，不能放普通设置。缓存只在内存：天气 5 分钟、新闻 5 分钟、搜索和 RAG 不缓存；缓存键包含配置 revision，更新 key/来源后失效。

### 异常与安全

连接3秒、请求总计10秒；响应体上限1MiB。禁止重定向并显式禁用reqwest自动重试；URL只来自桌面配置或提供方固定路径，模型不能指定host。公共服务只接受HTTPS；私有RAG/NewsNow可用用户明确填写的LAN地址（含IPv6回环/ULA），不能通过搜索结果跳到内网。HTTP401/403→`plugin_auth_failed`，429→`plugin_rate_limited`，432/433→`plugin_quota_exceeded`（retryable=false），超时→`plugin_timeout`，坏JSON/XML→`plugin_schema_invalid`；不自动换供应商或共享key，错误正文不回填。RSS拒绝DTD/自定义实体，处理CDATA及XML预定义/数字引用，解析完整XML后才返回、摘要去HTML；截断按JSON转义后计数，外层工具结果≤2000字符。检索内容不授予控制权限。

Metaso HTTP200应用错误按官方表分类：errCode401/403鉴权失败、4002额度耗尽、400/4009供应商拒绝`plugin_provider_rejected`且不重试、500/-500服务错误，未知码/缺字段为schema错误；不把错误正文、余额或会话数据回填模型。跨settings文件与多secret写入的故障原子性未在本次供应商范围验收，前端假native保存失败仅证明草稿保留；该存储故障项仍归NV11-4，不声称真实DB回滚通过。

### 验证标准

- 每家 mock 覆盖正常、缺字段、空结果、401、429、超时、超大响应和重定向；断言 key 不进日志/事件/工具结果。
- 列表与调用双重开关、voice permit 撤销、参数越界、来源白名单、RAG 数据集不能改写。
- 农历用已核实日期 fixture 覆盖春节、闰月、节气、年界和时区跨日；不只将实现结果复制为预期。
- 前端 Keep/Set/Clear、测试失败保留旧配置、结果截断与旧表单兼容；真实提供方握手/计费与数据质量独立验收。


**NV19 衔接**：复用本阶段 RAGFlow 固定数据集和只读来源机制，避免第二套重复调用（[NV19](#nv19)）。

<a id="nv12"></a>

## 4. NV12 视觉

### 技术框架

复用 8003 的 axum HTTP router 和设备 token 校验；新 VisionProvider 使用 OpenAI 兼容多模态 Chat Completions。视觉模型、地址和 key 独立配置，默认关闭；开启不等于授权 MOSS 自动调用设备摄像头。

### 修改位置

- 已新增 `crates/moss-voice-server/src/vision.rs`、`crates/moss-voice-providers/src/vision.rs`，在 8003 router 接入接口，修改 Shared/Config 与 `Session::start_device_mcp`；新增只读 `DeviceRegistry::authorize_http`，不会调用修改 last_seen 的 `seen`。
- 已修改 `VoiceSettings`、`voice_builtin.rs` 与 `frontend/src/components/VoiceVisionCard.jsx`：默认禁用、保存配置及使用合成图片的连接测试；密钥放 `voice.vision.main`。
- 已增加 server HTTP、provider 假服务、设备 MCP initialize 测试；更全面的上传畸形边界和真实设备固件 fixture 待 FINAL。

### 实现步骤

1. 先实现 VisionProvider request/response 与 fake provider，支持模型超时和取消；不把图像混进普通文本 Llm 接口。
2. 增加 `POST /mcp/vision/explain` 的鉴权、multipart 解析、大小及格式限制；在 body extractor 前由 middleware 调用 authorize_http 校验设备/client/token 和允许状态，得到可撤销 HttpDevicePermit。DB 查询在 blocking worker 中执行，不让上传请求修改设备身份登记。
3. 只向已允许设备的 MCP initialize 返回 `capabilities.vision={url,token}`；URL 从实际监听配置生成，禁用/缺配置时省略整项。
4. 请求校验后取得每设备/全局配额，再调用 provider；返回文字前再次校验设备未撤销。取消、撤销或连接关闭清理图片内存和配额。
5. 用设备端请求 fixture 验证上传字段和响应读取路径，再做 UI 模型连接测试。真实拍照另行授权，不用模型测试按钮触发摄像头。

### 数据与接口

```http
POST /mcp/vision/explain
Authorization: Bearer <设备 token>
Device-Id: <已允许设备标识>
Client-Id: <与配对记录一致的标识>
Content-Type: multipart/form-data; boundary=<自动生成>
```

multipart 仅有 `question`（UTF-8，1..2000 字符）和 `image`（单个 JPEG/PNG，解码前≤5 MiB，最大 4096×4096 且总像素≤1600 万）；总 body≤5 MiB+64 KiB，拒绝重复字段、额外文件、远程图片 URL 与 MIME/魔数不一致。实施时先对照所用固件的 multipart fixture；若其响应要求不同，调整兼容映射并同步本契约，不在服务端默默接受多种无限制格式。

provider body：`{model,stream:false,messages:[{role:"user",content:[{type:"text",text:question},{type:"image_url",image_url:{url:"data:<mime>;base64,<image>"}}]}],max_tokens:512}`，请求已有规范化的 `/chat/completions` 地址。设备成功响应 `{success:true,text:<最多2000字符>}`；失败 `{success:false,error:{code,message}}`。401 未认证、403 未允许/已撤销、413 超大、415 格式错误、422 解码或参数错误、429 忙、502 模型错误、504 超时；`Cache-Control:no-store`。

并发：全局 2、每设备 1，无排队，满额返回 429 和短 Retry-After；模型总时限 30 秒、响应≤1 MiB。上传及 Base64 只在有界内存中保存，请求结束即释放，不写数据库、临时文件、事件正文或对话历史图片。

### 异常与安全

实际授权契约 `DeviceRegistry::authorize_http(device_id,client_id) -> Result<Option<u64>,String>`：HTTP middleware 使用 TokenSigner 验证 device/client/token，后端只读查 allowed、client 与授权代次；HttpDevicePermit 在返回结果前后重验代次，不信任 Header 自报身份。模型故障不得改用普通 LLM 宣称“看见了”。视觉结果仅是资料，不自动发起运动/出光/家居控制；危险材料仍由现有门禁判断。限制解码尺寸防压缩炸弹，关掉 handler/body 调试日志；并发 permit 用 RAII 释放，异常和提前返回都不能泄漏。

### 验证标准

- HTTP mock：无/坏/过期 token、client 错配、撤销前后、重复字段、畸形图片、5 MiB 边界、像素限制。
- initialize：启用且配置有效才有 vision；设备收到的端口正确，日志及管理响应不回显 token。
- 配额：两个设备并发、同设备重复请求、满额、超时后释放、请求中撤销；fake provider 的请求字段和错误映射准确。
- 临时目录快照确认无图片落盘；真实模型、固件兼容上传、相机与端到端延迟分别验收。

### 本轮软件验收结果（2026-10-10）

Rust 全量 511/511、视觉 provider 4/4、HTTP 假设备 5/5、MCP initialize/桌面 ACL、前端单测与正式构建、FakeGrbl E2E 32/32 通过。Windows EXE/普通 NSIS 正式构建成功。Android aarch64 release 编译成功并报告 1 APK；Gradle 复用已有包导致额外时间戳断言失败，已验证 APK 内的 arm64 Rust 库 SHA-256 与本轮新编译库完全一致。更多坏图/像素边界、取消/超时和文件系统快照未全覆盖；真实固件、摄像头、视觉模型及安装包实机测试留 FINAL，不自动拍摄、不授权激光动作，普通 NSIS 不能代替 NV18 专用安装包。

<a id="nv13"></a>

## 5. NV13 Claude Code / Codex

### 技术框架

使用固定 executable、参数数组和管道启动已安装 CLI；新增 `ApprovalKind::AgentRun`、专用审批执行入口，复用现有声源许可、摘要、单次消费和审计。CLI 权限限制与 Windows Job Object 作用不同：前者约束模型工具，后者用于限制并终止进程树，不能把 Job Object 称为安全沙箱。

### 修改位置

- 拟新增 `crates/laser-host/src/voice_agent.rs`：能力探测、审批准备/执行、进程树和有界事件流；不要接到 ExternalMcp 的 ExternalTool 执行者。
- 修改 `laser-contracts/src/approval.rs`、`laser-api/src/approval.rs` 和审批卡，增加种类、request/target_key/digest 的穷尽匹配、请求展示与受信执行分支；RuntimeDb 增加 Agent 记录 CRUD。
- 修改 `VoiceSettings`、桌面工具组合和命令；拟新增 `frontend/src/components/VoiceAgentCard.jsx`，显示探测结果及最近 50 条记录。

### 实现步骤

1. 只读探测 `--version/--help`，保存支持的版本和参数 fixture；缺失关键限制参数即不暴露工具。2026-10-10 本机仅核对到 Codex 0.157.1、Claude Code 2.1.283 的帮助输出，尚未执行 Agent 或验证沙箱。
2. 校验固定工作目录：默认运行目录 `agent-workspace/`，拒绝系统目录、用户主目录根、运行目录根及包含 MOSS 数据/密钥的目录；canonicalize，处理重解析点和父路径逃逸。配置变化使待审批请求失效。
3. `agent_codex/agent_claude_code` 收到任务后只创建审批；保存任务完整文本、workspace、CLI 路径及版本、配置 revision、真实 voice permit 与 digest。审批卡显示任务和范围；不把任务插到 shell 文本。
4. 批准和启动前重算 digest、复核许可和 sandbox 能力；在同一事务中消费审批、创建唯一 run 并记 running/执行前审计，再启动进程。启动失败将 run 和 approval outcome 同事务记失败，不能恢复成可再次消费。使用参数数组、stdin 传任务，stdout/stderr 并发读取。Windows 以 Job Object 管理整个进程树，设置 kill-on-close 和最多 8 个子进程；不启动可见控制台。
5. 300 秒超时、用户取消、撤销或应用退出均停止任务并清理进程树；归档脱敏结果，按事件类型播报进度（最多每 3 秒一次），不能把模型描述冒充真实执行进度。

### 数据与接口

以下是**已核对参数存在的拟用调用**，实施时用 `Command::arg` 逐个传值；不复制为带真实目录/任务的 shell 字符串：

```text
codex --ask-for-approval never exec --sandbox read-only --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --cd <固定目录> --json --color never -
claude --print --restricted --permission-mode plan --permission-prompts none --tools Read,Glob,Grep --strict-mcp-config --mcp-config {"mcpServers":{}} --setting-sources "" --no-session-persistence --output-format stream-json --verbose
```

Codex 从 stdin 读任务，Claude 的无参数 print 模式从 stdin 读任务；后者须用 fake runner 和已安装版本 fixture 验证 stdin 行为后启用。Codex `never` 拒绝需要升级的请求，不是跳过沙箱；禁用 `--dangerously-bypass-*`、`--approve-for-me`、`--add-dir` 和写入模式。Claude 只提供 Read/Glob/Grep，restricted 不恢复 shell、写文件、网络抓取或 MCP；缺 `--restricted` 的旧版本不自动回退到更弱规则。参数依据 [Codex 官方命令文档](https://developers.openai.com/codex/cli/reference/)、[Claude CLI 文档](https://code.claude.com/docs/en/cli-reference) 及本机 help；官方与本机参数有差异时按已核实版本支持集执行。

子进程使用环境变量白名单（必要系统路径、locale、CLI 专属认证配置引用），清除 MOSS 设备、MCP、插件 key、配对 token、`MOSS_ENABLE_DEVELOPER_COMMANDS` 等变量。不读取并输出认证配置。`--ignore-user-config` 不等于停止读取 CLI 自身登录凭据；授权卡必须说明会使用已安装 CLI 的账号并可能产生模型费用。专门工作目录不允许放 `.env`、MOSS 库或 MCP 配置。

拟新增 `voice_agent_runs(id, approval_id TEXT NOT NULL UNIQUE, provider, workspace, task_digest, config_revision, status, output, truncated, started_at, finished_at, error_code)`；完整任务保留在 approval.request，不额外复制到普通日志。每条 run 关联唯一审批，执行结果、取消和重启恢复与 approval outcome 同事务更新。本地展示/保留最多 50 条 run，单条输出≤256 KiB；输出不回写系统日志。审批状态遵循现有生命周期，run 从消费后的 running 转 `completed|failed|cancelled|timed_out|result_unknown`；拒绝/过期未启动的请求仅留审批，重启时 running 标 result_unknown，不自动续跑。任务≤4000 字符；工具返回 `{status:"approval_required",approval_id,speech}`，执行结束后桌面记录可查看，语音只读结果摘要。

### 异常与安全

readonly CLI 不保证能隔离任意恶意可执行文件；只允许用户选定的已安装受信 CLI，CLI 的写入/命令/MCP限制必须通过权限负向测试。Windows 原生沙箱无法生效、管理配置重新开放写工具/MCP或测试发现能修改工作文件时，返回 `agent_sandbox_unavailable` 并停用该提供方，不能降到完整用户权限。路径规范化、固定 cwd、清空 MCP 和环境变量都不是单独的 OS 沙箱。stdout 单行≤64 KiB，累计≤256 KiB；超过时标截断并继续排空管道到退出，避免背压死锁。启动/读取失败→failed；超时必须观察整个进程树退出，不能只杀父进程。子进程永远没有 MOSS 工具连接，审批结果只允许一次 Agent 任务，不授予后续硬件权限。

### 验证标准

- fake runner：未批准零 spawn；任务、目录、CLI 或许可改变不启动；重复批准一次执行，拒绝/过期/撤销失效。
- 参数与隔离：任务包含引号、换行、反引号仍只是 stdin 数据；拒绝路径逃逸、符号链接/重解析点到敏感根；断言无 MOSS 环境或 MCP 继承。
- 进程夹具：大量 stdout/stderr、半行 JSON、断管、300 秒的可控时钟超时、带子进程取消、退出/重启未知结果、50 条保留和截断。
- 另经授权以无凭据临时目录验证真实 CLI 只读、命令与 MCP 禁止、进程树清理。只运行 help 不算该验收通过；真实 CLI 未测时保持 partial。

#### 2026-10-11 本机真实 Codex 沙箱负向测试（NV13-5，未通过，不解锁）

用户已同意无激光硬件情况下的安全测试。本轮真实二进制为 `codex-cli 0.157.1`，位于用户本机已安装的 Codex app-server 包目录（不向文档写个人绝对路径）。真实 `codex exec --help` 已核验 `--sandbox read-only`、`--ignore-user-config`、`--ignore-rules`、`--ephemeral`、`--skip-git-repo-check`、`--cd`、`--json` 和 `--color` 等限制参数。测试没有调用模型，没有读取真实账户配置；每次新建随机临时 workspace 与 `CODEX_HOME`，清空继承环境后仅保留 Windows 系统路径及测试临时路径。测试进程只操作人工 canary，命令退出后仅清理其自身随机临时目录。

| 验证项 | 实际结果 | 状态 |
|---|---|---|
| 原版 `codex sandbox -P :read-only` 启动 `cmd.exe` | 成功，退出码 0 | PASS：基本沙箱可用 |
| 读取工作区内 canary | 成功 | PASS |
| **读取工作区外**的兄弟临时 canary | **也成功读取** | **FAIL：读取隔离** |
| 写入工作区内 / 外的临时文件 | 进程拒绝访问，文件未生成 | PASS：写入拒绝 |
| 在沙箱中通过 `curl.exe` 连接本测试进程的 `127.0.0.1` 临时 TCP 端口 | 测试监听器接受连接；因未提供 HTTP 响应 curl 最终超时 | **FAIL：本机网络可访问** |
| 自定义 profile：`:minimal=read` + `:workspace_roots=read` + `network.enabled=false` | 拒绝启动并返回 `Restricted read-only access requires the elevated Windows sandbox backend` | **BLOCKED：缺高级 Windows 沙箱后端** |
| 300 秒超时、撤销、完整子进程树回收、MCP 隔离以及登录账号费用 | 未对真实模型/生产进程执行器测试 | UNVERIFIED |

结论：`-P :read-only` 只能证明写入受到限制，**不能当成独立的文件读取/网络隔离**。Windows 更严格权限 profile 尚不能启动，不自动安装或提权。因此本机真实 CLI 不能用于语音 AgentRun，生产继续锁定 `REAL_CLI_SANDBOX_VERIFIED=false`，桌面拒绝开启；剩余 NV13-3（Job Object、子进程/输出上限、取消和超时）与 NV13-5（隔离后再测真实模型）仍为未完成。无硬件连接只避免了激光风险，不改变这些本机权限结论。

#### 2026-10-11 Windows Job Object 纯软件进程夹具（NV13-3，生产仍锁定）

新增 `crates/laser-host/src/voice_agent/windows_process.rs` 和 `windows_process/tests.rs`。仅在本机临时目录中调用测试二进制，不启动真实 Claude/Codex；生产 AgentCoordinator 仍只使用 DisabledRunner。使用 `CreateProcessW` 挂起创建、`PROC_THREAD_ATTRIBUTE_HANDLE_LIST` 只继承 stdin/stdout/stderr、先分配 Job 并验证当前许可，再恢复主线程。`KILL_ON_JOB_CLOSE` 与 `ACTIVE_PROCESS_LIMIT(8)` 确保生命周期和数量受控，取消、voice permit 撤销、超时或正常退出均回收整个 Job 并检查进程统计，关闭 Job 句柄可杀掉成员。任务写入标准输入，stdout/stderr 同时排空；单行 64 KiB、合并总量 256 KiB（超出继续排空）、常见敏感关键词行保守遮盖，且环境变量只接受白名单，测试标记仅编译进测试版本。

Windows-only Rust 测试 **10/10 通过**：挂起期撤销绝不执行夹具、Job close kill、取消/超时/设备权限撤销、父子回收（额外重复 5 次）、第 9 个进程创建被拒绝、任务 stdin 及 argv 隔离、双管道洪泛/单行与总量截断、异常环境密钥拒绝。没有连接机器或使用用户真实 Codex 登录账号。**Job Object 不能限制文件读取与网络**；之前 Codex 普通只读沙箱可越界读取、连接本机端口，严格 restricted-read 因缺 elevated backend 无法启动，所以生产 `REAL_CLI_SANDBOX_VERIFIED=false`，真实 AgentRun 不开放。

真实模型/账号、OS 严格读取和网络隔离、CLI 异常崩溃、完整 300 秒实测及 OS 提权均未验收，后续需独立授权。
#### 本轮发布与测试归档

Windows 进程专项 10/10、父子进程取消回收额外连续 5 次，Rust 工作区全量 529/529，前端单测及正式构建、FakeGrbl E2E 32/32 均通过。Windows 正式 EXE SHA-256 `ac05a44ebaee92911022e357d22a75f248c84e878343d5afb084441986f8f76c`，普通 NSIS SHA-256 `a3916e191f18639238b3420e94c93de78cdcdf095ff27efe21c2981076d74e93`；Android 正式 APK SHA-256 `27fbe4d8240effad6d658bfea0a299300d885d2e990a24d40bd79ad2deaba518`，APK 内 Rust arm64 库与当轮编译 `.so` SHA-256 `55366fe977983539cf823df1fff238c4e2169a1b6d5b0350bfb9a973335cfb7b` 完全一致。发布构建成功 **不代表真实 Codex 执行或用户 Windows 文件/网络隔离通过**；保留 `REAL_CLI_SANDBOX_VERIFIED=false`。普通 NSIS 也不等于 NV18 带语音辅助程序的专用安装包。

#### 2026-10-11 Windows 环境只读诊断与 Docker 无模型隔离验证

- 只读诊断：Windows 25H2、build 26200.9457、`EditionID=CoreCountrySpecific`（家庭中文版），当前终端不是管理员。Hypervisor 已运行、VBS 状态为 2；WMI 显示 `VirtualMachinePlatform` 安装状态 1（已启用），`HypervisorPlatform` 状态 2（禁用）。由于 Hypervisor 已在运行，部分 CPU 虚拟化字段为 False **不能单独用于认定 BIOS 未开启虚拟化**。微软的独立 **Windows Sandbox** 不支持 Home 版，这和 **Codex Elevated Windows sandbox backend** 是两种不同的技术；`codex sandbox setup --help` 显示 `--elevated --current-user`，但本轮未运行安装/提权/ACL 修改，原有 setup marker 的存在不证明严格隔离已成功。
- 已安装 Docker 29.8.0 Linux 引擎（WSL2），本地已缓存 `ubuntu:22.04`，不需要下载镜像。新增回归脚本 `scripts/test-nv13-container-isolation.ps1`：只用随机的 Windows 临时目录，工作区只读绑定到 `/workspace`，**不挂载其他宿主机文件、Docker socket、MOSS 项目和用户目录**；Docker 参数 `--pull never --rm --network none --read-only --cap-drop ALL --security-opt no-new-privileges --pids-limit 8 --memory 256m --cpus 0.5 --user 65534:65534`。容器退出后只删除本轮创建的容器和临时目录。
- 首次人工实验及脚本重复执行均 **PASS**：允许读取已挂载的 canary，工作区外未挂载 canary 无法访问，向 `/workspace` 和 `/tmp` 写入均被拒绝，Docker socket 不暴露，默认网络路由不存在，容器无法连接测试程序在 Windows `127.0.0.1` 临时端口（Windows 监听器未接收到连接），cgroup `pids.max=8`，Linux capabilities 为 0，Seccomp 模式为 2，敏感 MOSS/MCP/OpenAI/Anthropic 环境变量未继承。脚本退出 0，宿主机 canary 未改变，测试容器和临时目录清理完毕。
- **严格边界**：以上只是 Docker/Linux 空镜像的离线隔离夹具，不是针对真实 Codex Linux CLI 和恶意程序的完整安全证明。Windows 原版 `codex.exe` 不能直接作为 Linux 原生 CLI 使用；正式 Agent 还需要独立安装/验证受信 Linux Codex 版本、隔离认证、MCP/插件禁入、可审计的**仅模型 API**受控外联，以及容器退出/取消与审批链联动。当前 `--network none` 不能让 Codex 访问模型。以上准备和系统或账号改动尚未获得授权，**真实生产仍保持 `REAL_CLI_SANDBOX_VERIFIED=false`、`DisabledRunner` 和禁用 UI**。
#### 2026-10-11 经授权安装官方 Linux Codex 0.162.1（Docker 离线验证，生产仍禁用）

- **官方来源及可复现安装**：从 `github.com/openai/codex` 官方 `rust-v0.162.1` 发布页下载 `codex-x86_64-unknown-linux-musl.tar.gz`，先读取 GitHub API 发布资产 `digest`/size 并与本项目固定值核对，再计算下载包 SHA-256，结果 `86f268d81b898f3e144c5ecff2ad2fda2c5802e042d6195b72538fe0fa125057`，大小 111,350,218 字节。解压的 Linux 二进制 SHA-256 为 `628e35487d888a28f0427fc37501da529dd3d81fcbad7a2c3421c1e8670d92e1`；不执行网络安装脚本、不复制 Windows `~/.codex`，也不下载新基础镜像。使用 `scripts/build-nv13-linux-codex-test-image.ps1` 基于本地缓存 `ubuntu:22.04`、Docker build `--network none --pull=false` 创建 **专用测试镜像** `moss-nv13-codex-test:0.162.1`，image ID `sha256:baa32539eac9ecca7831bac5848c0ecab8a0a84986c5822a4d371fffa7d435ac`、大小约 526 MB、Linux amd64、默认用户 65534。第一次构建 Dockerfile 版本标签断行而失败，已改用固定 here-string Dockerfile 并重新构建成功；构建临时目录仅清理本次创建的文件，镜像保留。
- **实际 Linux CLI + 外层容器隔离检查**：`scripts/test-nv13-linux-codex-image.ps1 -Image moss-nv13-codex-test:0.162.1` 运行 **真实二进制** `codex --version`（`codex-cli 0.162.1`）、`codex --help`、`codex exec --help`，均退出 0，不登录、不调用模型（不可写且不存在的 CODEX_HOME 引起非致命 PATH alias 提示）。同一个镜像中的离线 fixture 使用 `--rm --pull never --network none --read-only --cap-drop ALL --security-opt no-new-privileges --pids-limit 8 --memory 256m --cpus 0.5 --user 65534:65534`，唯一宿主机挂载是新生成的 `/workspace` 只读 canary；允许工作区 canary 读取，未挂载宿主机兄弟 canary 不可见，`/workspace` 和 `/tmp` 写入拒绝、Docker socket 不可见、没有默认网络路由和 Windows 127.0.0.1 端口连接、pids.max=8、capabilities 为零、Seccomp 模式=2、未继承 MOSS/云密钥环境或账户文件，停止睡眠进程的测试容器后可自动移除。离线脚本完整 PASS，随机临时容器和 canary 清理完毕，保留镜像供将来复测。
- **独立兼容性缺口**：另行以 `codex sandbox -P :read-only ...` 尝试 **Codex 内部二次 Linux 沙箱**：原始不可写 CODEX_HOME 被拒；改用存在的只读 synthetic workspace 后提示 bubblewrap 构造时遇到只读文件系统；进一步只开放**容器内** 16 MiB noexec/nosuid 的临时 `/tmp` tmpfs，仍因当前精简 Ubuntu 镜像中没有系统或随 Codex 分发的 `bwrap` 而失败（exit 101）。没有为测试放宽 Docker Seccomp/capabilities、安装 bwrap 或赋予容器特权。因此目前仅通过 **Docker 外层隔离**，不能声称 Codex 内置 Linux 二次沙箱可用；若后续要求内外双层沙箱，须另行受控安装和复测 bwrap。
- **未获准/未验证**：没有复制/使用任何真实登录会话、没有发出 Codex 模型 API 调用，也没有给测试容器外网访问、MOSS 项目挂载或激光设备访问权限。Linux CLI 的实际对话任务、MCP/插件禁入、受控 API 出站、独立认证、容器异常退出/宿主机权限策略以及与 NV13 单次审批/取消的生产整合 **均未完成**。无网络容器不能调用模型；生产端继续 `REAL_CLI_SANDBOX_VERIFIED=false` + `DisabledRunner`，桌面仍拒绝启用 Codex。
#### 2026-10-11 bwrap 安装 / 内层 namespace 阻塞与离线代理链路验证

- 使用 `scripts/build-nv13-codex-bwrap-test-image.ps1`，只在从 SHA 固定 Codex 镜像派生的 Ubuntu 测试镜像内安装 Ubuntu APT GPG 签名 `bubblewrap 0.6.1-1ubuntu0.3`。真实 `unshare --user --map-root-user` 非特权运行返回 `Operation not permitted`；真实 Codex `sandbox -P :read-only` 明确返回 `bwrap: No permissions to create new namespace`。**内层 Linux sandbox 仍 BLOCKED**，禁止提高 Docker 特权、关闭 seccomp、改 Windows 设置；不连接生产执行器。
- 使用 `scripts/build-nv13-fake-api-gateway-image.ps1`、`scripts/test-nv13-internal-model-gateway.ps1` 运行一个只返回人工固定 HTTP 200 内容的临时代理：Codex 镜像所在客户端只连接 `docker network --internal` 内的假服务，网络无默认外网路由，未连接本轮 Windows 测试监听器；无工作区写权限、宿主机凭据或 Docker socket，测试结束后容器、Docker 网络和 canary 清理。**该测试没有访问真实 OpenAI、没有模型费用/登录，也未验证 TLS、DNS/IP 防绕过、正式代理密钥隔离或跨进程撤销。**
- 后续受控模型代理、单次审批、外联授权与必须通过的负向测试列在 [`NV13 Codex Docker 安全设计与验收`](nv13-codex-docker-security.md)。Docker 基础隔离与假 HTTP 路径可用不等于“Codex 真实可运行”。

**NV19 衔接**：仅把 Agent 任务记录作为可检索的只读证据；不解锁 CLI，不重放 `result_unknown` 或已消费审批（[NV19](#nv19)）。

<a id="nv14"></a>

## 6. NV14 音乐

### 技术框架

本机文件索引 + [Symphonia](https://github.com/pdeljanov/Symphonia) 解复用/解码 + 单声道 24 kHz 重采样 + 现有 Opus 编码和 PlaybackPump。按锁定版本开启 mp3/flac/wav/ogg/vorbis/pcm 所需 feature；不下载音乐、不开外部播放器。音乐是 R0 的音频播放工具，播放状态由会话管理。

### 修改位置

- 拟新增 `crates/moss-voice-server/src/music/{mod,index,decode}.rs`；复用 providers 重采样实现，避免另写一套。
- 修改 `conversation.rs`、`session.rs`、`tools.rs`：拟新增受控的 `PlayMusic` 会话事件与 generation/cancel token；修改 Cargo.toml、VoiceSettings。
- 拟新增 `frontend/src/components/VoiceMusicCard.jsx`；桌面选择目录、刷新索引和状态命令沿用既有窗口边界。

### 实现步骤

1. 只索引默认运行目录 `music/` 或用户明确选择的目录；递归最多 8 层、10000 文件，不跟随目录重解析点，不读网络 URL。用相对路径生成 track_id，保存文件大小/mtime，不在工具清单暴露绝对目录。
2. 将文件名/标签规范化做精确、包含匹配；精确优先，同分多个候选返回最多 5 个供澄清，不能随意选一首；随机仅在索引有效文件中选。标签解析也有长度限制。
3. worker 解码并混音至 mono，连续重采样到 24 kHz；保留跨块状态，经有界 PCM 通道交给 session 的 `pump.push()`，由现有 pump 按 60 ms 在 `next_frame()` 编码 Opus。不让 worker 先编码造成双编码，不将整首加载进内存。
4. `PlayMusic` 事件切换会话播放源；原播报/音乐先 cancel 并清队列，同一设备仅一个源。音乐时禁用闲置断开计时，结束后恢复；新请求采用替换播放，不新增未定义的跨歌单队列。
5. 唤醒、abort、允许的自然插话、撤销、断线、服务退出都取消解码与队列；设备/轮次 generation 对不上就丢弃迟到帧。音乐不计入用户听过的 LLM 回答历史。

### 数据与接口

`play_music` 输入 `{query?:string,random?:boolean}`，两者恰选其一；query 1..200 字符。返回 `{status:"started",track_id,title}` 或 `{status:"ambiguous",candidates:[{track_id,title}]}`；找不到是 `music_not_found`，不存在或被移动是 `music_file_unavailable`。索引只在内存，最多 10000 项；磁盘配置只存根目录和开关，默认关闭。

`MusicPcm{device_id,generation,samples}` 使用单声道 24 kHz i16，通道按 PCM 时长最多缓存 2 秒；session 复核 generation 后调用 `pump.push()`，块长不必恰好 60 ms，由 pump 聚帧与编码。队列是当前曲目的 PCM 队列，首版不提供播放列表、跨会话自动续播或下载。打开文件后再次核对在根目录内、大小/mtime未变化；track_id 不能被调用方替换为任意路径。EOS 排空当前音频后发送 tts stop；取消立即清泵，不能等待 EOS。

### 异常与安全

损坏帧最多跳过连续 3 个，再报 `music_decode_failed` 并结束；不支持的编码明确报告，不能把所有 `.ogg` 当成可解码（Ogg 是容器）。worker 不阻塞设备循环，解码 CPU/缓冲有界；传输错误先 cancel，结果“播放已停止/连接已断”，不能报告整曲完成。采样率、声道数和时长元信息不可信，拒绝非有限采样和不合理声道/采样率。目录不可用不自动改到其他用户目录。

### 验证标准

- 临时目录索引：中文/空格文件名、同名歧义、大小写、损坏标签、10000 文件上限、移动文件和根外链接。
- 小音频 fixture：mp3/flac/wav/Ogg Vorbis 解码，立体声混音，跨块重采样连续性、60 ms 帧节奏和有限内存。
- fake 设备：首帧、正常 EOS、唤醒/abort/revoke 清泵、不播旧 generation、两设备不串音、自然插话开关保持原行为。
- 真实设备的音质、断音、长曲资源占用和插话延迟留独立验收，不用文件解码结果替代听音。

### 2026-10-11 NV14 软件实现与阶段验收

- `music/index.rs`：用户明确本机目录、8层/10000首/5000目录/50000项索引、单文件512MiB，拒绝敏感根、UNC、链接/重解析点；track_id 不暴露绝对路径，按实际打开句柄、size/mtime 再验后播放。中文同名、歧义、超限、移动文件、越界目录模拟测试通过。
- `music/decode.rs`：Symphonia 0.5 的 wav/mp3/flac/ogg/vorbis/pcm feature，流式混音及有状态24kHz重采样，最多20个×100ms PCM 队列、取消及输出界限；合成 WAV 与 Opus60ms、坏文件和满队列取消通过。**2026-10-11 已新增 MP3/FLAC/Ogg Vorbis 合成有效文件夹具并通过真实解码 24kHz PCM 测试，Ogg Opus 明确拒绝**，不能从编译 feature 推断已经支持每一种实际文件。
- `music/tools.rs`、`music/playback.rs`、`conversation.rs`、`session.rs` 已连接受控 `play_music` → `PlayMusic` 事件与一设备一播放源。旧 generation PCM 丢弃，切歌/abort/唤醒/自然插话/撤销清旧帧，发声前复核许可；音乐不写 LLM 历史。假小智 WebSocket 已测试点歌、Opus音频、abort 与撤销。
- 桌面 `voice_music_status/pick_directory/save` + `VoiceMusicCard.jsx`：新装与旧装默认 disabled；仅点击本机文件夹选择器后返回目录文字，保存启用时才扫描并重启内置服务。三条命令只授予 main 窗口，Android 不支持。
- 目前未做实体音质/长歌/打断延迟测试，也没使用真实个人音乐文件；NV14-5 保留 FINAL。跨模块完整构建结果另记 CHANGELOG。

本轮软件回归 Rust 572/572、FakeGrbl 32/32、前端测试/构建、Windows EXE/NSIS 和 Android aarch64 APK release 成功（校验值见 CHANGELOG），且控制台提示词预算1925/2000、知识库171/200；旧安装音乐默认关闭。正式真机声音与长歌压力、实际设备打断延迟继续留 NV14-5 / FINAL；MP3、FLAC、Ogg Vorbis 有效测试文件已在 NV14-2 软件补测，解码专项 6/6 成功。

### 2026-10-11 NV14-2 有效音频格式软件补测收尾

- 完全合成的 1 秒 440Hz MP3、FLAC、Ogg Vorbis 已通过 Symphonia → 24kHz mono PCM 格式回归；Ogg Opus（未支持）和截断 FLAC 负向拒绝也通过。测试文件/复现脚本在 `crates/moss-voice-server/tests/fixtures/`、`scripts/generate-nv14-synthetic-audio-fixtures.ps1`，不包含真人录音/个人歌曲，产生脚本使用临时离线 Docker FFmpeg。
- 最新完整回归：Rust workspace 587/587、前端测试/build、FakeGrbl E2E 32/32、Windows EXE/普通 NSIS 和 Android aarch64 APK release 均 PASS；Android 原生库 SHA-256 与本轮编译一致，详情见 CHANGELOG。NV14-5 小智设备真实听音、长曲、插话时延等仍在 FINAL/unverified；不需要提前要求用户手动测试。

<a id="nv15"></a>

## 7. NV15 Home Assistant

### 技术框架

`reqwest` REST 客户端 + `app_secrets` 长期令牌 + 桌面实体白名单 + 新增 `ApprovalKind::HomeControl`。复用可信 voice permit 和审批摘要，控制默认 Ask；lock/alarm_control_panel/cover 永远 Ask。免批只对用户逐实体开启的低风险服务生效，不受激光严格模式开关控制。

### 修改位置

- 拟新增 `crates/laser-host/src/voice_home.rs`：HA 客户端、实体策略和 HomeControl 执行者；工具组合接入 hass 三工具。
- 修改 `laser-contracts/src/approval.rs`、`laser-api/src/approval.rs`、审批卡：家居语义、参数和结果；VoiceSettings 保存 endpoint/实体策略/版本，token 放 `voice.plugin.home_assistant`。
- 拟新增 `frontend/src/components/VoiceHomeCard.jsx` 与对应桌面命令；不把现有 `laser-device` 的 HA 未接入占位改成无条件成功。

### 实现步骤

1. 桌面测试 token 后 `GET /api/states` 同步实体，再取 `GET /api/services` 得到可用服务；仅勾选实体提供给语音，可设置别名，重名要求澄清。
2. 实现状态查询，返回状态、last_updated 和允许属性；每次从当前白名单取 entity_id，不接受任意模型域名/实体。
3. 固定控制映射如下表，拒绝其他 domain/service；`hass_set_state` 名字保留，但实际控制必须调用 services，不能用 POST states 伪造状态。
4. 默认创建 HomeControl 审批，绑定实体、服务、规范化参数、endpoint/token revision、实体策略和 voice permit。批准及发送前重查；用户移除实体、换 token/地址或撤销设备，待审批立即失效。
5. 成功后查询目标状态，只报告 HA 接受及观测状态；断线/超时标结果未知，不自动重发。前端显示服务不可用和实体 unavailable，不静默取消白名单选择。

### 数据与接口

| 语音动作 | 实际 HA 请求 | 限制 |
| --- | --- | --- |
| `hass_get_state {entity_id}` | `GET /api/states/{entity_id}` | 白名单查询，unknown/unavailable 原样说明 |
| light/switch on/off | `POST /api/services/{domain}/{service}`，domain 为 light 或 switch，service 为 turn_on 或 turn_off | body 的 entity_id 由白名单填写；light 亮度 0..255，额外参数拒绝 |
| climate 温度 | `POST /api/services/climate/set_temperature` | 有限数字且位于实体 min_temp/max_temp 内；没有边界则拒绝而非猜测 |
| lock/alarm/cover | lock 的 lock/unlock；alarm 的 arm_home/arm_away/disarm；cover 的 open_cover/close_cover/stop_cover | 仅服务列表真实支持的动作；始终批准。需要 PIN 等额外 secret 的动作首版不开放，不让模型提交 PIN |
| `hass_play_music` | `POST /api/services/media_player/play_media` | 仅勾选播放器和桌面保存的媒体 ID/类型白名单；不允许任意外链 URL |

映射采用 [Home Assistant REST 文档](https://developers.home-assistant.io/docs/api/rest/)：POST states 只修改 HA 表示，控制实体需调用 services。请求 Bearer token、JSON body；返回 changed states 可能含其他实体，所以不能把整个数组当作目标执行证明。服务响应后再读目标，最多 3 秒、3 次观测；未达到目标只返回 `accepted/state_unconfirmed`，不重发控制。

`hass_set_state` 输入 `{entity_id,action,parameters}`；枚举动作由本地表限定。设置实体项 `{entity_id,alias,enabled,policy:"ask"|"allow"}`，高风险域不能保存 allow；配置 revision 递增。审批摘要包括请求、目标、策略和来源，不含明文 token。锁/报警控制若支持状态但不支持服务仍标不可控制。

### 异常与安全

连接 3 秒、总计 10 秒，禁止重定向，响应≤1 MiB；401/403 提示重新设置令牌，404 实体不存在，429 受限，网络失败不清掉原配置。只请求 states/services，不实现模板、脚本、shell、任意事件或任意服务转发。Ask 的创建/执行审计失败即拒绝；Allow 也记录脱敏目标和结果，执行前的必要审计失败不发送。HomeControl 不借用激光 `confirmed=true` 或 ExternalTool 免批策略。

### 验证标准

- mock HA 验证所有映射、参数边界、changed states 混入无关实体、服务缺失、状态 unavailable 和断线。
- 高风险实体无法 Allow；未勾选、移除、重名、变更 key/endpoint/策略、撤销、重复批准都不能误执行。
- 未批准零 POST；发送前许可失效零 POST；超时可能已经执行时返回 result_unknown 且请求计数为一次。
- 前端同步/勾选/别名/审批/状态提示，以及三端回归；真实家居动作需逐实体另行授权验收。

### 2026-10-11 NV15 离线状态/服务同步与审批策略验证

- `crates/laser-host/src/voice_home.rs` (未对生产公开)：只接收可信同步得到的 ≤1MiB states/services JSON 与显式桌面实体选取表，要求实际存在的域与固定服务、别名唯一和实体 allowed；排除所有其他域、未知/未勾选实体、system namespace，并只保留安全展示属性。
- `voice_home/mock_http.rs` 完全 `cfg(test)`：临时回环假 HA 用固定 GET /api/states/services 返回合成 JSON；重定向、401、HTML、过大、断线、伪造 URL/方法等负向校验，不读取用户真实 HA 配置。
- 控制计划严格映射本技术书 §7 服务表，未允许任意 URL、Shell/模板、POST states、额外参数、缺少温度边界的气候控制、未在本地白名单的媒体 ID；所有返回 `requires_dedicated_approval=true`，**不会发出任何 POST**。
- `voice_home/approval_mock.rs` 为内存模拟审批，测试 30s TTL、32字节摘要身份 ticket（随机令牌哈希）、一次性消耗、设备身份及 permit、配置/密钥 revision、当前服务重核验；不是实际 ApprovalKind::HomeControl，真实持久审批/凭据托管/UI/状态观测及真实模型工具联动仍未接线。模拟被批准的计划也不会执行。
- Rust `cargo test -p laser-host voice_home --lib` 12/12 PASS，其他回归/构建结果见 CHANGELOG。此阶段为 `partial`，不需要连接任何家庭设备。

### 2026-10-11 NV15-4 网络回执断线的安全结果处理（仅 mock）

- 已存在 `ApprovalKind::HomeControl` 专用持久审批和 `voice_home/control_mock.rs` 的 fake socket 被用于测试；本轮补强 **一次 POST 之后**不论断线还是响应非预期均返回 `ResultUnknown`，再通过持久化 `ApprovalOutcome.phase=Unknown` 锁定终态，绝不将网络错误当作未执行而自动重发。
- 回读最大三次固定 `/api/states/{entity_id}` GET，截止时长 3 秒；只确认当前授权目标实体，不能用 changed states 数组中其他实体代替。授权失效或计划被篡改则零 POST。`cargo test -p laser-host voice_home --lib` 21/21 PASS，无实际家居设备访问；生产 HomeControl 执行器、凭据管理、真实设备仍保持禁用。
### 2026-10-11 NV15 白名单实体 ID / 别名歧义防护（离线）

- 原先 `HomeCatalog::lookup` 以别名优先，且桌面配置/目录均没有阻止 A 设备别名等于 B 设备的完整 entity_id；未来若误接生产执行器，显式请求 B 的 ID 可能反而控制 A。当前执行器依旧默认禁用，未产生真实家居动作。
- 现在桌面配置连暂时关闭的候选实体也预留其完整 ID，拒绝跨实体 alias 与 ID 同名、大小写重复 ID，且禁止任何别名伪装成尚未加入白名单的合法 HA 实体 ID；模拟目录对启用条目再次检查，查精确 ID 时优先命中自身，并拒绝仅空格别名。自己的 entity_id 允许作为自身别名；没有放宽 HomeControl 审批和控制禁令。
- 安全回归：`laser-host voice_home` 22/22、`moss-voice nv15_` 3/3 PASS；生产 token、同步、独立审批 UI 与真实 HA 仍 unverified。

### 2026-10-11 NV15 离线配置 revision 条件写入

- `voice.builtin_home_revision` 是有限安全整数，旧配置默认 0；读写失败、损坏的当前配置和过期 `expected_revision` 全部拒绝写入。单独更新白名单不会覆盖 MCP、记忆或其他配置，通用 VoiceSettings 保存也不能复活已撤销的 HA 白名单。无变更保存不递增版本。
- 主窗口离线配置页显示版本及 409 冲突提示，要求用户明确重新加载最新白名单后再编辑，不自动重放旧数据。既不存 token，也没有连接真实 HA、开放状态同步或控制。

### 2026-10-11 NV15 令牌管理与只读 HTTPS 连接器（实际设备仍未测试）

- `app_secrets` 新增 HA token 和 credential_revision 事务性条件写入、撤销 pending HomeControl 审批；状态接口不返回令牌，独立于 `settings.json`。该 SQLite 表目前**没有证明系统级静态加密**，不能把“存储隔离”称为完整的凭据安全验收。
- 新增 main-only 桌面令牌设置/撤销和手动只读状态同步；只请求固定 HTTPS `/api/states`、`/api/services`，连接3秒、总10秒、1MiB、无代理与重定向、严格200 JSON，结果按已勾选实体过滤，缺失实体展示 unavailable，过滤敏感属性，查询后检查 policy/token revision，变化丢弃。
- 保持 `can_control=false`、不执行服务 POST、不将锁/警报免批；当前 Windows loopback WebView 主窗口 ACL 信任边界仍需处理，真实 HA TLS/Token/状态/审批 UI 尚未实测，NV15-1..4 继续 partial。

### 2026-10-11 NV15 DPAPI / 安全窗口 / 持久控制结果内核增量

- `home_secret.rs` 以 Windows 用户 DPAPI 密封 Home Assistant token，新版本不明文落入 app_secrets；旧版明文在数据库打开时单次事务迁移，同时令旧 HomeControl 审批过期。对过期 WAL、外部备份无法承诺擦除，应提示用户安全销毁旧备份或轮换 token。其他平台 fail-closed。
- `VoiceHomeCard` 已迁入唯一 `voice-private` 打包窗口，四个 Home Tauri 命令都由 capability + 原生窗口 URL/标签双重限制；远程主窗口不能读写 HA 白名单或 token。增加显式勾选、每 15 秒一次的只读同步，关闭窗口及配置/令牌变更即停止。无任何真实服务 POST。
- `laser-host::voice_home::approved` 复用已存在的 HomeControl 事务单次消费与记录结果，在发送前校验冻结 path/body、白名单/凭据 revision、live permit；发送后最多观察目标状态三次、未知不重试。`https_transport` 有正式受限 HTTPS GET/POST 数据面接口，但**没有生产工具注册，也未接入真实家居控制执行**。两项是待进一步接入的可复用代码，不等于 NV15-2/3/4 验收完成。
- 当前 Windows/App、FakeGrbl、Rust 验证结果及未完成工作以 CHANGELOG 为准；真实 HA 凭据、DNS 地址切换/服务端 TLS、完整审批 UI、语音 tool 注册和设备动作均未验收。NV15 保持 partial。

### 2026-10-11 NV15 已审批控制结果观察撤销竞态修复（生产控制仍禁用）

- 单次 HomeControl 提交后的目标 GET 不再仅在发送前验证 voice permit，而是在 **GET 完成后也检查许可与 3 秒总时限**，撤销中返回的旧成功状态只能成为 `ApprovalPhase::Unknown/result_unknown`，既不能确认完成，也不能重发 POST。每次 HTTPS 观察请求接受剩余时间预算，最多 900ms。
- 本机合成 TLS 测试使用临时自签证书、`127.0.0.1:0` 模拟 HA 路由和假令牌，覆盖正确/错误主机名、POST 固定服务路径、GET 精确目标、redirect/HTML/错实体/超大响应/慢响应拒绝；`laser-host voice_home` **31/31 PASS**。这仅是软件安全验收，不等于真实 HA 网络/审批 UI/实体动作已许可，NV15 仍 partial。

<a id="nv16"></a>

## 8. NV16 声纹

### 技术框架

本地embedding在GPL helper提取，不链接进MIT主程序。模型已固定CampPlus `campplus_sv_zh_cn_16k_common`（官方模型卡v1.0.0、Apache License2.0），实际28,281,138字节、SHA-256 `f682b514c05d947ee3fa91cd6ec6c5c7543479a128373fa29b1faedccd21fd11`、16kHz/192维，已加入模型清单/下载界面。完整协议与边界见 [登记说明](nv16-speaker-enrollment.md)。身份识别和个人记忆注入仍关闭，合成推理不作为真人标定。

### 修改位置

- 已修改helper、协议与providers：协议4、speaker独立线程/宿主独立进程和Windows模型文件锁。
- `speaker.rs`保留离线合成策略；实际当前句快照/历史/摘要在`person_context.rs`，接`conversation.rs`和`session.rs`，桌面`DesktopIdentity/DbPersonStore`适配helper和SQLite。
- RuntimeDb schema9已增加说话人和独立按人记忆表；与NV10设备摘要分离，未知者不回退，不自动迁移共享摘要。
- 已新增 `speaker_enrollment.rs`、`VoiceSpeakerCard.jsx` 与私有 `voice_speaker_manage`，SQLite事务登记/删除。加工身份限制尚未接入，身份不得由模型自报。

### 实现步骤

1. 先以临时音频 fixture 探测候选模型，锁定模型版本、hash/许可证、维度、16 kHz 输入和 CPU/内存；必须达到可测资源预算后才进入下载列表。
2. 登记收集 3 段独立提示语，或用户选定 WAV；每段有效语音 2..10 秒，去静音后提 embedding，逐向量归一化再平均归一化。全零/NaN、太短、削波或多说话人样本拒绝重录。
3. 识别与ASR并行，同设备最多1任务、全局2、无识别等待队列；普通句在ASR之后有界等待从识别启动起1.5秒，超时unknown，停止/退出先返回。阻塞helper计算另持许可至真实收尾。按人摘要队列≤16、并发2、模型30秒超时；排队后发送文字前和提交前再验身份/授权。可选“仅登记者”加工限制未实施，不改变现有加工审批。
4. 分模型做阈值标定，保存 threshold 与 top1/top2 margin；匹配不足/分数接近均 unknown。姓名作为数据注入，不能变成系统指令。
5. 按人记忆独立存储，不自动把设备共享摘要迁给任何人。删除人同时取消任务、删除 embedding 和对应人摘要、作废未消费的身份相关审批；关闭声纹停止计算和人摘要注入。

### 数据与接口

已实现 `voice_speakers(id,name,embedding,model_id,dimension,revision,created_at)`及 `voice_person_memory(speaker_id PRIMARY KEY,summary,revision,updated_at)`；embedding固定192维f32编码，核对有限值/维度/模型，内部模板不序列化。个人摘要条件更新，未知者不读取/写入登记者摘要。设备记忆保留，按人模式不回落共享摘要。详细命令、状态机和本轮验证见[NV16登记说明](nv16-speaker-enrollment.md)。

识别结果 `{speaker_id?:string,score,margin,model_id,utterance_generation}` 只由本地受信路径产生；任何 tool arguments 里的 name/score 均无授权效力。默认声纹关闭、默认仅登记者限制关闭；测试起点 `threshold=0.70,margin=0.05` **只是调试候选，不作为发布阈值**。需实测至少 3 人、每人 20 段同人和 20 段异人对比，覆盖安静/噪声/距离/重放；发布门禁值由标定报告给出。无足够真人数据时，身份提示可标 experimental，加工限制不得宣称已可靠验收。

外部 `/voiceprint/identify` 是可选兼容适配。NV16-1 必须固定目标服务版本的鉴权、音频编码及请求/响应 fixture；契约未核实不显示可用。启用前说明音频会发往用户所选服务，本地关闭不自动退回外部服务。

### 异常与安全

声纹不是可靠身份认证，录音重放可能被识别；仅登记者限制用于减少误发起，不替代审批。开启该限制时，注册者的加工仍要桌面批准，即使全局严格模式关闭也不能用姓名免批；这是该可选限制本身的规则，不修改关闭此限制时的现有执行行为。已在运行的加工不因身份超时自行取消；所有停止通道不等待或要求声纹。原始录音只在内存，登记导入仅使用用户选定文件，不另存副本；向量按模型绑定，更新模型后提示重新登记，不偷偷换维度。

### 验证标准

- 纯向量：余弦已知结果、阈值两侧、margin、零/NaN/错维度、同分与模型不一致；模拟时钟验证并行和超时。
- helper：新旧协议握手、3 段登记、加载失败、关闭/删除时旧 generation 不回填，不保存原始录音。
- 门禁：未知拒绝加工、已注册仍审批、模型伪造 speaker_id 无效、审批中删除/撤销失效；停止始终可调用。
- 真人标定分别给 FAR/FRR、噪声及重放误识别结果，不把纯向量单测当成真人通过；缺模型/真人时记录具体未验项。

### 2026-10-11 NV16 纯合成声纹策略门禁（不连接模型/设备）

- 新增 `moss-voice-server/src/speaker.rs` 私有离线策略层，用固定合成向量模拟三段独立声纹登记、64..1024 维有限值和单段 2..10s 元数据、三段成对相似度及归一化；拒绝削波/多声源、模型或维度不同、旧轮次、相近两位候选，默认匹配 `Unknown`。调试用 0.70/0.05 **不是经过真人校准的可发布阈值**。
- 不调用麦克风/helper，不读取个人录音、不持久化 speaker 或记忆、不发送外网，不向工具/审批公开任何 speaker ID；无论匹配结果如何 `may_authorize_action` 都为 false。全局 2/同设备 1、等待队列 16 只作为不启动计算的内存资源策略。Rust `speaker` 4/4 合成测试通过，正式声纹功能仍未启用，真人标定保留 FINAL。
### 2026-10-11 NV16 离线按人记忆模拟

- `speaker.rs` 新增私有 SyntheticPersonMemory 合成版本控制隔离夹具，验证两名不同已登记者、未标定与未知身份、删除失效和旧 revision 写入拒绝；无 SQLite、原始音频、真实模型或生产身份授权通路。模型自报身份仍不得成为授权。Rust 专项 5/5 PASS，真实 NV16-1～4 仍 pending。


**NV19 衔接**：按人记忆须仍满足校准的可信声纹、当前设备许可及身份代次。生产 `Calibration=None` 时不开放真人按人检索（[NV19](#nv19)）。

<a id="nv17"></a>

## 9. NV17 固件下发

### 技术框架

复用已有 OTA router、`OtaInfo`、`FirmwareOffer` 和 `is_newer_version`；新增本地文件登记、SHA-256、逐型号推送确认和受保护下载。目前 OTA 固定 `firmware:None`，只有基础响应与设备登记，固件下载尚未实现。NV17 不自动降级/回滚，恢复设备刷机由用户另行授权。

### 修改位置

- 拟新增 `crates/moss-voice-server/src/firmware.rs`，修改 `ota.rs`：候选固件和下载路由。
- 修改 RuntimeDb：固件登记/推送状态、每设备下载/上报记录；服务端 Registry 提供当前允许设备/client 绑定查询。
- 桌面导入/推送确认/停用命令及拟新增 `VoiceFirmwareCard.jsx`；配置、错误码和下载契约同步文档。

### 实现步骤

1. 用户选 `.bin`，填写 board.type 和版本；只读用户选定文件后复制到 runtime firmware 的本任务 staging，计算大小与 SHA-256，再原子提交文件和登记。同型号同版本不同 hash 冲突，不覆盖既有发布记录。
2. 导入默认 disabled；启用弹窗显示型号、版本、大小、hash、将影响的已登记设备数。确认后保存具体版本/hash和 revision；重新导入、改文件或换型号使确认失效。
3. OTA 先验设备/client/允许状态和可信身份凭据，再取对应型号已启用候选；仅凭 Device-Id/Client-Id 自报字段不能授予固件下载。复用数字逐段比较，仅更高版本提供 offer。缺记录/不可比/同版本/更低版本返回原来的无升级响应。
4. 增加 `/xiaozhi/ota/download/{filename}`，只从登记白名单映射已验证文件，不按 URL 任意拼路径；下载前重新验 hash 和当前推送/设备授权，并限制并发。
5. 把“提供 offer”“下载结束”“设备再次上报新版本”分别记录；服务重启/断线不把下载等同安装成功。关闭推送撤销后续 offer 和新下载，正在进行的文件传输取消，但不声称能撤回已开始的设备刷写。

### 数据与接口

拟新增 `voice_firmware(id,model,version,filename,size_bytes,sha256,enabled,revision,created_at)` 与 `voice_firmware_downloads(id,device_id,firmware_id,status,started_at,finished_at,error_code)`。设备上报版本沿用 voice_devices 现有字段；下载时间不能覆盖上报版本。文件名由规范化 ASCII 型号标识和数字段版本生成；型号允许字符之外用不可逆安全编码映射，登记仍保存原始 board.type 作精确匹配，不能因清洗名称合并不同型号。

`FirmwareOffer` 保留 `{version,url}`，不假设固件会消费额外 SHA 字段。下载鉴权在 NV17-1 先核对当前固件 HTTP 实现：若能携带设备 Bearer/header，复用设备 token；若下载不能带 header，但 OTA 检查已能证明持有有效设备 token，可签发限定 device/client/firmware/hash、有效期 5 分钟的下载参数并服务端实时复核允许状态。OTA 检查本身也不能证明身份时，不给自报 ID 签发下载凭据，保留无升级响应并明确标出需设备侧兼容改造的阻塞。URL 整体属于凭据，不进日志或普通管理响应。未完成固件请求 fixture 验证时禁止打开推送。

首版单文件≤16 MiB、总量≤256 MiB，超限导入失败并提示用户选清理对象，不自动删除旧固件。下载响应 `application/octet-stream`、准确 Content-Length、ETag=hash、Cache-Control:no-store；不支持 Range 就明确返回 416，不能假装续传。全局下载 2、每设备 1，无排队；失败重试由下一次设备请求决定，服务端不主动刷机。

状态：固件 `staging → disabled → enabled → disabled`；校验损坏转 `invalid`。设备观察 `offered → downloading → download_complete → observed_new_version`；断流 `download_failed`，新版本未上报保持“更新结果未确认”，不写 flashed/completed。二进制是否适配板型不能仅由用户填写型号或 SHA 证明。

### 异常与安全

防路径穿越、URL 编码混淆、重解析点、重复文件名和 TOCTOU；下载使用已打开且校验的文件句柄，不能验 hash 后再按路径重开可能已被替换的文件。先写 staging/hash再事务登记，崩溃残留由精确登记范围恢复；无完整记录不提供 offer。鉴权、型号、hash、版本或推送状态变化立即拒绝；不含 `force`、不自动降级、不删除已启用文件。传输失败不改变设备的已上报版本，失败恢复不能宣称设备未被刷写。

### 验证标准

- 临时文件：导入、重复/冲突、哈希损坏、限额、原子提交失败/重启恢复；路径穿越、编码、链接和并发替换。
- mock OTA/download：未允许/client 错配、同型号/不同型号、同版/补零/预发布/非法版本、默认关、确认变更失效、签名过期/撤销。
- 下载中断、客户端取消、并发上限、Range、hash 与准确字节；download_complete 后不上报不能标成功。
- 固件实机刷写和断电恢复需要具体设备授权及设备侧恢复方案，未操作时 `unverified`；本计划不增加自动回滚。

### 2026-10-11 NV17 合成固件安全登记（OTA 服务保持无升级）

- 新增私有 `moss-voice-server/src/firmware.rs`，只从已打开的测试输入流在临时目录 staged/hash/硬链接无覆盖登记；16MiB 单件、256MiB 总量、数字段版本规范化，型号 hash 独立文件名。每一项默认 disabled；启用需要相同 sha/revision。变更/哈希损坏拒绝合成下载；非法设备当前版本一律不给 offer。
- 4/4 Rust 合成测试通过，输入仅 fake bytes 和 `tempfile`，**未接 OTA 路由、未验证设备 token 或固件内容真实性、未读取用户选择文件**。仍需真实设备请求格式、严格身份校验、下载许可签名和取消状态链；NV17-5 固件刷写需要用户另行批准。
### 2026-10-11 NV17-3 离线一次性下载票据与中途撤销测试（不接 OTA）

仅在 `#[cfg(test)]` 下新增 `firmware/download_mock.rs`。模拟可信设备许可方签发 32 字节 OS 随机票据，只在内存保留 SHA-256 摘要，绑定已确认固件的 board、version、sha、record revision，以及 device/client/credential revision、最长期限 5 分钟和可撤销的 session 原子许可。票据单次消费，全局最多 2 个同时下载、每设备最多 1 个；以 64 KiB 合成块拷贝给闭包测试接收者，每块检查过期/许可/身份，撤销后不继续后续输出。文件字节仍由原 `ArtifactStore::read_fake` 对照 size 与完整 SHA，篡改拒绝。模拟传输完成**不**写入设备已刷机状态。

**测试结果**：`cargo test -p moss-voice-server firmware --lib` **8/8 PASS**，覆盖正确绑定、重放、错 client、密钥版本变化、到期/撤销、禁用固件、文件被改、全局/设备并发和接收期间中止。测试不创建下载 HTTP 路由、不读取真实设备请求，不发固件 offer，更不会刷机；仍需正式 DeviceRegistry 登录身份、受信 OTA 请求 fixture、持久记录和权限 revocation 设计及验证。此轮仅测试代码与文档，未改变生产编译/协议，依项目规则可不执行 Windows/Android 重建。
**NV17-1 设备请求身份核查结论**：当前 `moss-xiaozhi-proto::OtaInfo::from_request` 从 OTA 明文 `Device-Id` / `Client-Id` 请求头和 body 读取设备身份、型号与版本（默认还会回退到 `default` / `0.0.0`），**这些都是客户端自报数据，没有加密签名或凭据校验**。`moss-voice-server/src/ota.rs::ota_post` 在解析后会调用 `registry.seen` 登记并在已允许时**签发** WebSocket token，但 OTA 请求本身并未提交用 `TokenSigner::verify` 核验的 Bearer 凭据。因此，即使数据库里该设备当前是 allowed，也**不能将这个 OTA POST 单独作为签发固件下载授权的身份证明**。后续下载如果设备端不支持附加签名/header，需要独立可信绑定流程和具体固件请求 fixture；无法验证时保持 `firmware:None`。上面的 mock ticket 接收的 `FakeGrant` 只是可信边界的测试替身，不得从 HTTP 头直接构造后用于真实发放。未连接设备、未发送固件。
<a id="nv18"></a>

## 10. NV18 收尾与交付

### 技术框架

沿用 Tauri 2 NSIS、现有安装/卸载钩子和三端 release 构建。新 `tauri.voice-sidecar.conf.json` 已声明 `externalBin=["binaries/moss-voice-local"]`；`scripts/build-windows-installer.ps1` 已构建并收集 Windows helper EXE。**2026-10-10 当前进度：专用脚本已统一根 target，已暂存 helper 自身及 path 依赖源码、COPYING 与初版第三方说明，overlay 已配置资源；本次新的 NSIS 文件存在但执行任务状态丢失、manifest 未生成，尚未解包和安装证明实际含 helper。静态依赖的全部对应源码/许可核对仍缺，不能声明发布合规。**

### 2026-10-11 NV18 最新软件证据（不代表已发布）

- `scripts/build-nv18-sidecar-safe.ps1` 现在以按次独立暂存/动态 Tauri overlay 构建；旧 `build-windows-installer.ps1` 已安全转交到新脚本，原有未提交 `bundle-resources/voice-local` 不会再被递归删除。独立 release EXE、GPL helper 和带 helper NSIS 均有本轮成功回执，真实大小/hash 见 CHANGELOG；新版 `target/nv18-24d485ff5a9f4bfb8e4d7c958dbb6fa6/release-manifest.json` 核对协议3、DB schema8及输入 hash。配置与源暂存路径是构建 ID 唯一的 task-owned 输入，不是必须部署到用户电脑的路径。
- `scripts/verify-nv18-nsis-in-docker.ps1` 使用任务专属、运行时断网、只读挂载的 Ubuntu 官方 p7zip-full 解析当前 NSIS，**22 个条目包含主程序、语音 helper、COPYING、THIRD_PARTY_NOTICES 和对应源码 main.rs**，证据是独立压缩包列举，不只是 Tauri 配置或文件存在。临时检查镜像和容器清理完成。`WinRAR.exe` 无法得到可靠命令行结果，后续不以该尝试作证据。
- **剩余必须验收**：第三方原生依赖完整许可/对应源码（目前 notices 明确 preliminary），完整源码临时重建、Windows 干净机安装/升级/卸载与 sidecar 协议启动、Android 实物及小智音频听音；签名和外部分发另行取得批准。用户尚未授权安装，更未进行设备操作。通用 `target/release` 与本轮带 helper 的 `target/x86_64-pc-windows-msvc/release` 两套构建文件不能混用。
### 2026-10-11 NV18 完整软件回归、解包标记校验与对应源码复建

- 专用 sidecar NSIS / 标准 NSIS / Android 全套已成功新建，Rust workspace 609/609、前端构建/单测、FakeGrbl 32/32 均 PASS。新版 NSIS 验证器严格检查全部七类包内文件与敏感文件禁入，helper hash 完全一致；NSIS 内 desktop 只有 3 个 Tauri bundle-type marker 字节与打包前 EXE 不同，在内存中重新 SHA-256 完全匹配，验证器 exit 0。
- 真实从暂存对应源码重新构建 GPL helper PASS（但 exe 位级 hash 与发布包原 helper 不一致，reproducible builds 未通过）。Cargo metadata 147 个 crates 有许可证字段，但 espeak-ng/native 对应源码和原生许可证清单未审完整；软件构建 ≠ 法律上可外部分发或干净系统安装通过。完整路径/哈希/安全限制请参见 [`NV18 源码/许可证审计`](../developer/nv18-source-license-audit-2026-10-11.md)。
### 修改位置

- 修改 `scripts/build-windows-installer.ps1`、`apps/desktop/src-tauri/tauri.voice-sidecar.conf.json`，必要时安装器 hooks；不直接手改 Tauri 生成结果。
- 已增加发布暂存 `apps/desktop/src-tauri/bundle-resources/voice-local/{source/,COPYING,THIRD_PARTY_NOTICES.md}`，由脚本生成并映射 Tauri `resources`；禁止把完整 helper 目录连同 target 缓存打进包，第三方全部许可及对应源码仍待验。
- 修改 `apps/moss-voice-local/README.md`、`THIRD_PARTY_NOTICES.md`、[安装规范](../developer/packaging-install.md) 与技术书版本引用；运行时 helper 定位沿用当前同目录规则。

### 实现步骤

1. 先核对版本清单：Tauri 产品版本、桌面/host crate 版本、helper 版本与协议、模型目录、数据库 schema、前端构建号；产品发布号一致，独立协议/schema 明确映射，不强行让所有数字相等。
2. 改脚本让 helper 使用根 `target`（`--target-dir <仓库>/target`），检查本次 build 退出码、时间、非空、`--version`；复制为 Tauri 输入 `moss-voice-local-x86_64-pc-windows-msvc.exe`，发布安装后应名为同目录 `moss-voice-local.exe`。
3. 暂存源码清单：helper 的 Cargo.toml/Cargo.lock/src、根协议 crate 所需源码、实际 path dependencies、构建说明和依赖版本清单；保留正确相对结构、COPYING 和 THIRD_PARTY_NOTICES。按依赖许可证补所需对应源码/许可资料，不用“只带一个 src 文件夹”宣称已满足现有交付要求。
4. sidecar overlay 增加 `bundle.resources` 映射，将暂存源码置于安装资源 `licenses/voice-local/source/`、许可置于 `licenses/voice-local/`；实际目录以 Tauri resource_dir 为准。安装器显示组件与许可证，卸载清程序资源但默认保留 runtime 用户数据，不删除自选音乐/Agent/导出目录。
5. 完成软件门禁与三端构建后，解包检查文件清单，再在获授权的干净 Windows 环境验收安装/启动/升级/卸载；Android 验证仍为远程模式，不打包 Windows helper。签名及外部分发单独取得发布授权。

### 数据与接口

交付清单 `release-manifest.json`记录实际产物文件名、大小、SHA-256、`helper_protocol`、`db_schema`和源码归档hash；本轮协议4/schema9由源码提取，未生成的产物不能伪填。清单不包含本机私有路径、token或地址，交付以本次最终版本为准。

```text
Windows 安装目录/
  moss-desktop.exe
  moss-voice-local.exe
  <其他 Tauri 运行依赖>
  <resource_dir>/licenses/voice-local/
    COPYING
    THIRD_PARTY_NOTICES.md
    source/<保持可构建相对结构的源码与构建说明>
```

本地 helper 仍经 `current_exe().parent()` 定位并做 hello 协议校验；当前已明确诊断缺失/旧协议，架构错按启动失败呈现。NV18 拟新增与发布清单关联的 helper hash 校验及架构专用诊断，不把它们描述为现有能力，也不回退查 PATH 上的任意同名程序。packaging manifest/hash 必须与同一构建产物关联，不能拿以前时间/哈希记录当本次证据。使用专用脚本的目标 triple 会生成 `target/x86_64-pc-windows-msvc/release/`；交付检查另保留项目要求的 `target/release/moss-desktop.exe` 常规 release 构建，分别记录对应 NSIS 来源，不覆盖成同一产物描述。

### 异常与安全

任何 helper/前端/桌面 build 失败即停止打包，不复用旧暂存 EXE；先构建暂存并校验再替换最终输入，失败时不把半份资源交给 bundler。运行中安装/升级/卸载只提示退出，不强杀 MOSS 或 helper；正常关闭由宿主协调并等 helper 退出。升级备份数据后迁移失败应明确报错，不删原库；降级需校验旧程序是否支持目标 schema，不能以“旧版都能打开新库”作 NV10 以后保证。应用软件回退不包含固件自动降级。

### 验证标准

- 构建：Rust workspace、独立 helper、前端单测/回环 E2E、frontend build、Windows `cargo tauri build`、带 helper 专用脚本、Android release APK；逐一记录命令结果和实际产物路径/hash。
- 解包：同一 NSIS 含正确架构 helper、源码、COPYING/第三方许可、资源映射；源码在临时环境能按说明构建，不夹带 target、用户数据、音频、图片或凭据。
- 安装：中文/空格自定义路径、当前用户/所有用户、缺 WebView2、helper 启动与协议、无 helper/旧 helper 错误提示、软件退出进程树；需要系统变更的测试另行授权。
- 升级与回退：保留设置/材料/设备/密钥/记忆；迁移失败保留数据；旧包恢复到备份数据后可启动，不让旧程序直接误用不支持的新 schema。卸载保留/显式删除用户数据、外部目录不删除。
- Android 与真实设备分别验收；签名、干净 VM、真实账号或硬件缺证据就逐项列未验，不用三端构建代表安装或实机通过。


<a id="nv19"></a>

## 11. NV19 长期记忆与智能上下文（规划中，尚未实施）

### 技术框架

复用 NV10 `MemoryService`/`VoiceMemoryStore`、原 `voice_memory` 摘要数据，NV11 `search_rag`/RAGFlow，NV16 `PersonContext` 的可信身份边界，`laser-runtime` SQLite 及现有 Tauri 私有管理窗口；新增可选 `ContextService` 负责条目/来源/版本、异步候选提取、受信检索和上下文预算。**旧 NV10 路径独立可用；新能力默认关闭**。OpenViking 仅供参考，不作为 NV19 的默认运行依赖。

OpenViking 的 Resource/Memory/Skill 分类与 L0 目录摘要、L1 目录概览、L2 原始资料按需加载可做逻辑索引。L0/L1 属于目录级，不给每份普通文件生成三份副本。检索是一次带过滤的搜索：L0 粗筛、L1 重排并决定是否读 L2，不做逐层下钻或父子分数传递。技能只是可选索引信息，不授予工具或设备操作权限。

### 修改位置

- `crates/laser-runtime/src/db.rs`（或按约定拆出 `context_store.rs`）：新表、索引、迁移、CAS、清理、备份和按 scope 过滤。审查时 schema=9，实施从当时真实最新版本分配下一迁移，严禁覆盖旧迁移。
- `crates/moss-voice-server/src/memory.rs`、`person_context.rs`、`conversation.rs`、`session.rs`：记忆候选提取和 prompt 前受限检索；保留快速急停、语音播放和会话许可机制。
- `crates/moss-voice-server/src/plugins/rag.rs`、`plugins/runtime.rs`：复用 RAGFlow 现有只读白名单，新增本地目录索引与外部结果来源隔离。
- `apps/desktop/src-tauri/src/voice_builtin.rs`、`crates/moss-voice/src/settings.rs`、`frontend/src/components/VoiceMemoryCard.jsx`：私有窗口 CRUD、候选确认、来源/过期状态、使用量和明确开关；不在普通窗口/LAN 提供私人内容管理。
- `crates/laser-runtime/src/agent_store.rs` 的 AgentRun 保持原有用途。NV19 跨会话只读索引不得改变 NV13 正式 Runner 的 disabled 状态。

### 实现步骤（唯一任务状态见 TODO）

1. **NV19-01 结构化模型**：确定 preference、device_fact、technical_note、task_context、troubleshooting_experience 五类记录及来源、scope、版本、候选/确认/作废/到期状态；对设备、个人、项目、工作区明确权限矩阵。
2. **NV19-02 SQLite 与 CRUD**：增加独立记忆表/索引与受信接口，版本条件更新、删除/索引清理、导出/备份、迁移失败回滚；原 NV10 摘要不强制搬迁或删除，旧数据导入只生成候选。
3. **NV19-03 自动候选提取**：在用户显式授权后仅处理已提交且截断的会话文字，使用异步有界队列提取稳定事实；敏感过滤、来源校验与编辑/清空/撤销后的旧任务失效；验证经验需要用户或可信测试证据确认。每次提取写一份变更清单（新增、修改前后、删除、被拒绝操作及原因码），支持整批撤销。设备参数、材料实验和校准结果不提取进记忆，以现有表为准。
4. **NV19-04 检索**：先复核可信 scope，再按类型/时间/状态过滤，用 FTS5 `trigram` 检索（少于 3 字退回 `LIKE`），Top-K 和 Token/延时预算；设备、材料、校准问题直接查现有表。分快速模式（语音默认，不经大模型）和深度模式（桌面私有页面，可规划查询并重排）；闲聊、急停、NV8a 本地快捷命令跳过检索。注入仅作带来源的只读数据，失败回到 NV10 原路径。
5. **NV19-05 分层知识库**：仅索引用户明确选择的安全目录或文件，为目录生成 L0 摘要（约 256 字）和 L1 概览（约 4000 字），L2 为原文按需读取，检索时一次过滤搜索而非逐层下钻；版本/哈希和重命名/删除失效；RAGFlow 作为可选白名单后端。
6. **NV19-06 故障经验**：记录设备/固件条件、故障、排查、结论、验证证据及适用范围；未经确认的建议不能标成事实或触发激光/HA/OTA。
7. **NV19-07 跨会话只读恢复**：摘要固定为决策、待办、证据、未验证项四节；恢复时复核真实代码、数据库与运行状态，`result_unknown`/旧审批/停止的进程绝不重放。
8. **NV19-08 UI/全端验证**：私有页面增加分类搜索、编辑、删除、确认候选、来源、备份、空间与模型成本提示；覆盖权限、性能、迁移、回归与软件交付，真人/设备独立验收。

### 数据与接口（待实施的候选契约，不是已存在的数据表）

```sql
CREATE TABLE context_memories (
  id TEXT PRIMARY KEY NOT NULL,
  scope_kind TEXT NOT NULL,       -- device/person/project/workspace
  scope_id TEXT NOT NULL,         -- 由受信宿主生成，不接收模型自报身份
  category TEXT NOT NULL,
  content TEXT NOT NULL,          -- 脱敏有界文字，不存原始对话和音频
  verification TEXT NOT NULL,     -- candidate/confirmed/invalidated
  source_kind TEXT NOT NULL,
  source_ref TEXT,                -- 内部安全引用，不含凭据或原始敏感路径
  revision INTEGER NOT NULL DEFAULT 0,
  created_at REAL NOT NULL,
  updated_at REAL NOT NULL,
  expires_at REAL,
  deleted_at REAL
);
CREATE INDEX context_memories_scope ON context_memories
  (scope_kind,scope_id,category,verification,deleted_at);
```

- 内部暂定 `ContextService::retrieve(trusted_scope, question, budget)`；可信身份来自当前设备授权/已校准声纹/桌面明确选择的项目，不能从模型 JSON、资料或 Skill 推断。私有 CRUD 按 revision 条件更新，冲突返回可恢复错误。
- 提取变更清单候选表 `context_extraction_runs(id, scope_kind, scope_id, started_at, adds_json, updates_json, deletes_json, skipped_json, reverted_at)`，与记忆表同一迁移；撤销按清单逆向应用并做 revision 校验。
- 目录索引与记忆表分开维护；`libsqlite3-sys 0.30.1` 内置 SQLite 3.46.0 已开启 FTS5，使用 `tokenize='trigram'`，中文检索质量仍以固定问答集验证；不提前承诺向量检索。资料版本或访问权限变化时失效，删除须清理衍生索引与缓存。
- 外部模型处理和目录导入必须有显式用户选择、提供方与数据范围提示。历史摘要/检索结果只能标记为引用数据，不能写入授权/工具系统指令。

### 异常与安全

- **默认与隐私**：新增自动采集/索引/云端上传默认关闭；严禁存储原始音频、完整转写、密钥、审批、工具隐藏参数、敏感目录及可执行危险命令。继续保留旧 NV10 的开关与行为。
- **多身份隔离**：每次读取及异步落库重验设备许可、scope、版本、开关代次和修订；NV16 当前 `Calibration=None` 绝不能开启生产按人索引。撤销、编辑、清空、切换项目后旧任务/缓存不能恢复删除数据。
- **安全边界**：不可信资料不得覆盖系统规则、批准工具或改变 `CallerContext`；激光、急停、Home Assistant、OTA、MCP、Codex/Claude CLI 均维持原独立门禁；已消费审批或结果未知的任务绝不重跑。
- **外部依赖**：不强制运行 OpenViking/Docker/独立向量库。OpenViking 服务需 Python 3.10+/uv（Windows 走 Docker）及 embedding 与视觉语言模型；作为独立服务不改代码时义务很小，打进安装包即为分发，需满足主项目 AGPLv3。启用前验证 API/版本、离线/费用、资源与安全、备份恢复。

### 验证标准

- **迁移/兼容**：当前真实 schema 副本、空库、重试、崩溃中断、回滚、CAS 并发及 NV10 CRUD 全部回归，旧摘要原样保留。
- **隔离/安全**：设备/可信个人/项目相互查询不得越权；假 LLM/恶意文档/敏感日志/撤销后迟到结果均不能泄露或自动执行。
- **效果/成本**：固定中文问答集覆盖同义表达、两字短词、过时资料、冲突设备版本、找不到资料及来源核对；闲聊和急停轮次不得触发检索，快速模式不调用大模型；记录 Top-K 命中、来源正确、延迟 p50/p95、Token、内存/磁盘与原 NV10 对照，不能预填指标。
- **软件/实机分离**：新功能开发后按 AGENTS.md 更新 CHANGELOG、Rust 全量、前端、适用 E2E、Windows/Android release 与关联权限文档；真实语音、模型、设备、安装和发布均另行验收，不从模拟通过推断 PASS。

**关联**：[技术书 §15](moss-native-voice-spec.md)、[实施 TODO NV19](moss-fusion-implementation-todo.md)。本次只增加规划；原 NV10～NV18 既有进度保持不变。

## 12. 资料核验与实施时的再次确认

外部接口核对日期：2026-10-10。上述官方链接用于固定本次请求方向，实施前仍须对照用户实际版本创建 fixture 并锁定依赖。NewsNow 服务版本、可达 RSS 源、声纹模型 hash/许可证、外部 voiceprint-api 与固件下载鉴权能力属于明确的首步核验项；缺项只阻止该适配器或真实验收，不伪造结果、不阻止其他已授权的软件工作。

文档修改本身不运行三端构建、不操作 CLI Agent/摄像头/家居/固件；开发完成后的测试与交付要求仍按各节和 AGENTS.md 执行。
