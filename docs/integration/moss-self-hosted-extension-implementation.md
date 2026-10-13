# MOSS 外部自建链路 P2b、P3、P5 实施细则

状态：实施方案 s1（2026-10-10），只是技术文档，不代表功能已实现。P2b、P5 可选；P3 只针对外部 xiaozhi-esp32-server，不影响内置语音。设计依据见 [融合技术书](moss-desktop-fusion-technical-spec.md)，进度见 [实施 TODO](moss-fusion-implementation-todo.md)。

## 0. 开发入口与共用约束

1. **现有主链路**：xiaozhi-server 与 MOSS 的 `laser-mcp::run_bridge` 均以客户端身份连接 MCP 接入点。只读的 P2a 验证和 P1 执行层审批是外部自建链路的前置；P2b 网关不是默认必做，只有需要审批结果语音播报、会话级幂等/审计，或接入点实测不兼容时才考虑启用。
2. **可信身份**：已有 `laser_contracts::CallerContext::{Gateway,McpBridge,Approval}` 与 `laser_api` 的 `require_r3_authority`；`laser_mcp::tools_list_for`、`dispatch_tool_call_with` 是现有工具入口。新路由只在认证后构造 Gateway 身份，决不信任模型自报的 `caller`、`confirmed`、`approval_id`。R2 停止走现有实时写入通道，不排在 G-code 队列后；R3 执行必须过统一门禁；R4 拒绝。
3. **现有外部服务端设置**：`crates/moss-voice/src/{models,config_yaml,center}.rs` 与 `apps/desktop/src-tauri/src/voice.rs` 已有模型测试和服务端配置保存能力；外部服务端模型密钥写入其自己的 `data/.config.yaml`，**不得**混入 MOSS 的 `app_secrets`、`settings.json` 或日志（内置语音使用 `app_secrets` 的规则另算）。
4. **数据库与状态**：开工时确认 `RuntimeDb` 实际 schema 版本。P2b 若实施，只追加当时可用的新迁移，不把新表塞回历史 v4；审批、幂等、审计都要有断电/崩溃恢复测试。不能将软件请求已发出冒充物理动作已完成。
5. **验收约束**：所有拟新增文件、类型和测试仅是实施计划。假 HTTP、假 MCP、`FakeGrbl` 和临时 SQLite 先行；外部服务部署、真实账号、模型、设备运动/出光分别需授权。代码阶段按 `AGENTS.md` 同步 CHANGELOG、契约/安全文档、测试及前端/Windows/Android release 构建；无实机证据标 `partial` 或 `unverified`。

<a id="p2b"></a>

## 1. P2b 可选网关与 Python 插件

### 技术框架

在 MOSS `laser-api` 增加受令牌鉴权的 HTTP 网关，使用现有 `laser-mcp` 工具清单/分发、`laser-api` 审批、`laser-runtime` SQLite 存储。上游 xiaozhi-esp32-server 的插件用 Python `@register_function(..., ToolType.SYSTEM_CTL)`、`async def` 和 `speak_txt`。网关默认关闭；开启时 `MOSS_GATEWAY_ENABLED=true` **必须**伴随 `MOSS_FUSION_STRICT=true`，后者的现有配置检查不可移除。插件和现有接入点不可对同一物理请求双发。

### 修改位置

- **P2b-00**：拟新增 `crates/laser-api/src/gateway.rs`，从现有 `ApiService` HTTP 入口挂载 `/v1/gateway/tools`、`/v1/gateway/tools/call`、`/v1/gateway/approvals/{id}`；响应遵守 [融合技术书 §5](moss-desktop-fusion-technical-spec.md) 的包装/错误约定和 S1 来源规则。
- `crates/laser-contracts/src/{lib,approval}.rs`：拟增 `GatewayCallRequest`、响应/错误类型，复用 `CallerContext::Gateway`、工具风险分级、已有 `ApprovalRecord`。`crates/laser-host`：需要时提供具有 Gateway 上下文的 `HostBackend` 适配，不能冒用 `Desktop` 或 `Approval`。
- `crates/laser-runtime/src/{db,config}.rs`：下一可用 schema 迁移、网关令牌 SHA-256 摘要/轮换和 `gateway_requests`；不得覆盖现有审批表或 `app_secrets`。
- **P2b-01**：在**用户提供并授权修改的** xiaozhi-server 独立仓库中拟新增 `plugins_func/functions/moss_tool.py` 并注册 `Intent.function_call.functions`。MOSS 仓库可保存不带凭据的 fixture/配置范例，但不能默认写用户自备外部目录。
- **P2b-02/03/04**：提示词只更新自建模式的网关话术；桌面令牌管理如需新增，只开放给可信主窗口。Android、局域网页面与模型不能获得令牌明文。

### 实现步骤

1. 先锁定 xiaozhi-server 与 MCP 接入点提交，核对 P1 的 `CallerContext`、工具级别和审批执行入口。使用 CSPRNG 生成网关令牌（首次显示原值，持久仅存哈希），实现停用与撤销；**认证成功后**才从受限字段生成 `CallerContext::Gateway { session_id }`。网关与严格模式开关不允许危险组合。
2. 工具目录来自 `tools_list_for` 当前有效白名单，R4、桌面命令、未知风险工具不得出现；调用时再次检查分级和禁用状态。删除请求 `arguments.confirmed` 并审计；R3 必须只生成/消费桌面审批，不能让网关直接批准；R2 降险指令仍走原始优先通路。
3. 对 `(token_hash, request_id)` 建原子唯一键，先持久记录 `processing` 再分发；重复键返回原结果或 `result_unknown`，相同 ID 却不同工具/参数返回 409，绝不执行第二次。进程在外部动作后崩溃时保持未知，重启不自动重放；10 分钟幂等窗口不是物理 exactly-once。
4. 插件从 `GET /v1/gateway/tools` 获取描述并构造函数；`approval_required`、`target_required`、拒绝、过期、结果未知分别播报。轮询 `GET /v1/gateway/approvals/{id}` **只查状态**，不得在 `approved` 时重新提交加工。真正执行结束且仍属当前会话才允许 `speak_txt` 播报；断线/切换会话不恢复旧播报。
5. 提示词仅补充网关路由、桌面现场批准和降险词，明确模型不具备确认资格；旧官方/内置链路提示词保留。网关启用与接入点并存的状态要在界面显示清楚，避免用户重复提交同一作业。
6. 联调前由用户确认部署/网络/令牌；先用假插件验证，再验证受控真实服务。若失败，关闭可选网关并恢复原接入点，**不**自动恢复旧动作或改变现有审批结果。

### 数据与接口

沿用 [融合技术书 §5](moss-desktop-fusion-technical-spec.md)：
- `POST /v1/gateway/tools/call`：`Authorization: Bearer <gateway_token>`，请求 `{session_id,request_id,tool,arguments}`。`session_id` 为审计关联，**非身份凭证**；`request_id` 由客户端生成 UUID，重复提交必须具有相同参数摘要。
- `GET /v1/gateway/tools`：返回工具白名单、当前开关与风险标记，不含 R4/未知风险工具。
- `GET /v1/gateway/approvals/{id}`：返回脱敏的审批状态、到期与结果摘要，不能返回连接明文、完整路径或隐藏请求。
- 计划表 `gateway_requests(token_hash,request_id,session_id,tool,args_digest,status,response,created_at,updated_at)`，主键 `(token_hash,request_id)`，状态 `processing|completed|denied|result_unknown`；同一事务完成插入/消费的关键转换。实际字段与新版 schema 在 P2b-00 核对并写迁移测试。最多 30 调用/会话/分钟，30 秒工具预算；参数和响应上限实施时与实际服务/工具契约对齐，不直接接受不受限 JSON。

### 异常与安全

没有有效令牌→401；禁用→404；工具不允许→403；同幂等键不同请求→409；限流→429；显式目标缺失→`target_required`；超时/掉线且可能已执行→`result_unknown`，告知现场确认且不自动重复操作。网关令牌不放 URL、日志或工具结果，不与配对 token/桌面会话复用；远端访问必须先评估 TLS 和认证，不能以“只在局域网”作为免鉴权理由。幂等/审计失败不能放行 R3；R2 真实急停路径不依赖可选网关存储成功。Python 插件和命令执行并非沙箱；任何工具响应都不能授予新权限。

### 验证标准

- 模拟鉴权/令牌轮换、来源、禁用、R0/R1、R2、R3、R4、限流、重复幂等键/参数冲突、崩溃窗口/结果未知、审计失败；覆盖 A-10..A-13、A-20、A-25、A-28。
- 假 xiaozhi 插件：动态工具目录、过期/拒绝/结果未知、只读轮询、不同会话结果不能串播、绝对路径和密钥不外传。
- `FakeGrbl` 比较接入点与网关两个入口的 R3 最终门禁，不把模拟器通过写成真实出光；设备和实际部署需另获授权。

<a id="p3"></a>

## 2. P3 外部自建服务端云端模型切换

### 技术框架

复用 Python xiaozhi-esp32-server 的 `type: openai` provider 与 MOSS 已有的模型测试/YAML 读改写；**逐项 ASR → TTS → LLM**，每项测试后才提交并重启自己启动的外部服务端。原生 Anthropic 协议不是该上游已核实的 `type: openai` provider，不能直接替代其字段。P3 不修改 MOSS 内置语音 `voice.builtin_models`。

### 修改位置

- **主要更改在外部服务端** `main/xiaozhi-server/data/.config.yaml`，MOSS 只按已实现的 `crates/moss-voice/src/{models,config_yaml,center}.rs` 和 `apps/desktop/src-tauri/src/voice.rs` 进行测试/保存/启动控制，前端复用 `frontend/src/pages/VoicePage.jsx`。
- 新需求必须先确认既有代码接口缺口，再修改测试/配置 DTO、相应 fake provider 测试及 `docs/developer/api-contracts.md`；不复制外部服务端含真实 key 的 YAML 进仓库。
- 验证材料记入 `docs/integration/self-hosted-model-bench.md`（如不存在先创建不含真实地址/密钥的模板），包括版本、格式、首包/总时延、工具调用准确率和真实/模拟验证级别。

### 实现步骤

1. P0 云端 ASR/LLM/TTS 能力核实、P2a 接入点实际握手及 P1 严格模式完成是正式切换的前置。锁定 Python 服务端提交和用户当前配置；先私下备份 `data/.config.yaml`，确认是否被 `manager-api.url` 智控台配置覆盖。
2. **P3-01 ASR**：确认服务支持完整 `/audio/transcriptions`、采样率、输入格式和中文；先用短样本测试连接，成功后只改 `ASR.MossASR` 与 `selected_module.ASR`，重启后确认文字、延迟和中断。失败只回滚 ASR。
3. **P3-02 TTS**：核实完整 `/audio/speech`、音色/语速/编码；测试后只改 `TTS.MossTTS` 与选择项；重启确认中文播报、可打断、无跳字并记录延迟。失败只回滚 TTS。
4. **P3-03 LLM**：测试 `/chat/completions` 对 `tools`、流式 `tool_calls` 聚合和中文的支持；只改 `LLM.MossLLM` 与选择项。使用固定测试句覆盖状态、材料、预览、批准询问、拒绝和停止，统计当时实际曝光工具的选择与参数准确率；不达用户确认标准则缩小模型**可见**工具集重新测，不能放宽后端门禁。
5. **P3-04 密钥/回滚**：每个分区遵守先测试再保存，空白配置不清除原值；按行改写保留其他键，备份 `.bak`、写后重新解析。智控台托管时仅测试、不写文件。外部运行的服务端不能被 MOSS 擅自停止，改后提示用户手动重启。
6. 验证成功的分区保留并串联下一阶段；失败恢复该分区原设置与仍有效的接入点。官方和内置模式不应因 P3 切换被修改；未获授权不发送真实 R3 指令。

### 数据与接口

依照 [融合技术书 §9.4.4](moss-desktop-fusion-technical-spec.md)：
- `LLM.MossLLM`：`type: openai`、`url`（基础 `/v1` 地址）、`api_key`、`model_name`。
- `ASR.MossASR`：`type: openai`、`base_url`（**完整** `/audio/transcriptions` 地址）、`api_key`、`model_name`、`output_dir`。
- `TTS.MossTTS`：`type: openai`、`api_url`（**完整** `/audio/speech` 地址）、`api_key`、`model`、`voice`、`speed`、`output_dir`。
- `selected_module` 分别指定 `LLM/ASR/TTS`，保持 `Intent: function_call`、`mcp_endpoint` 和其他现有键不变；Anthropic 原生 provider 需独立评估和实施。
- 测量记录 `{provider_kind,model_alias,api_family,input_format,first_response_ms,total_ms,tool_call_ok,verification_level,error_code,recorded_at}`；不保存密钥、私人地址、原始语音或对话全文。测试连接 15 秒且不跟随重定向；30 秒 MCP 工具超时另行检查。

### 异常与安全

401/403 提示检查认证、429 提示服务限额、5xx/超时标不可用；缺少函数调用/格式能力不擅自猜 endpoint 或自动切备用付费供应商。YAML 原子替换失败保留旧文件及原运行进程；密钥仅存自备服务端的私有配置和短期测试内存。模型返回 `confirmed=true` 不产生 R3 执行权；提示词、服务端日志、仓库和验收报告不能泄密。对 `tool_calls` 的失败或超时不自动重复运动/出光。

### 验证标准

- fake ASR/TTS/LLM 分别验证正常、401/429、超时、格式不兼容、密钥脱敏、YAML 备份/原子写/保留无关键、智控台只读。
- 每切换一项记录 A-01..A-04 和 A-14 可执行的验证；工具选择准确率按当前可见的真实列表统计，记录未通过的类别和版本。用现有假设备回归 R3 桌面审批/R2 停止；真人语音、云端账号和真实设备缺任一证据就标 `unverified`。
- 切回旧模型和官方模式后原用户配置、材料及安全门禁保持不变；未验证不得把“有 TLS/401 响应”写成云模型功能验收成功。

<a id="p5"></a>

## 3. P5 可选 xiaozhi-client / 海门扩展（未决定实施）

### 技术框架

P5-01 可选 xiaozhi-client 的 MCP 聚合能力；P5-02 可选海门 Agent 作为**外部工具提供方**，不让第二个 Agent 接管同一设备音频。先和现有 NV9 第三方 MCP 以及计划中的 NV13 Claude/Codex CLI 对比，只有存在明确不可替代的收益且用户确认才进入代码实施。

### 修改位置

- P5-01 先锁定 xiaozhi-client 实际提交、许可证和传输协议；尽量只改外部聚合器/接入点配置。确需 MOSS 适配时限制在 `crates/laser-mcp` 或独立的受限适配层，继续以可信 `McpBridge/Gateway` 身份进入。
- P5-02 锁定海门的真正对外工具协议；若需桥接，放独立服务，不把它变成第二个 `moss-voice-server` 设备 WS 会话。P0-09 决定和差异/兼容结论写入实施 TODO。

### 实现步骤

1. **P5-01** 源码核实 xiaozhi-client 的发现、工具重名、分页、禁用、会话路由、错误和许可证；与 NV9 能力做差异表。若无明确增益，可标 `[-]` 并说明不做原因。
2. 确定工具命名/源及权限白名单，防止同一工具同时在接入点、NV9 和网关被重复注册或执行；不得让聚合器读取未授权的密钥/目录。
3. **P5-02** 核实海门是否真能以工具提供方运行、以及它的只读/命令/MCP 边界；若必须并行监听设备音频则拒绝此方案，不破坏单一语音编排者规则。
4. 用户单独决定实施后再按固定版本创建可执行子任务，先 mock 再真实联调；未决定时 P5-01/02 仍保持 `[ ]`，不能视为产品缺陷或已授权开发。

### 数据与接口

拟最小记录 `{provider_id,source_type,version,tool_alias,policy,config_revision,last_error}`；若仅涉及外部聚合器配置，不新增 MOSS 表。工具名、开关和身份在列表与调用入口都验证，模型自报 `session_id` 不能成为身份或审批凭据；R3 仍由 MOSS 执行层审批。上游工具 `readOnlyHint` 不等于自动免审。

### 异常与安全

连不上、重名、协议不兼容、来源撤销或检测到双重音频编排时，关闭该可选适配器而不影响既有接入点；断线后的不确定执行不补发。海门 Agent 不继承 MOSS 激光工具或敏感环境变量，未知风险工具默认禁用；不得用 P5 替代 NV13 的受控 CLI 权限方案。

### 验证标准

假 MCP/Agent 逐项验证工具分级、冲突、关闭/撤销、双编排拒绝、R3/R4 门禁、断线与结果未知不重放。真实版本、服务端与设备联调需另行授权；缺来源或未决定做则标 `unverified`，不能宣称 P5 已交付。
