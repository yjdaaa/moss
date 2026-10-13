# Changelog

代码修复与功能变更记录，格式参考 Keep a Changelog；条目要求见 [`AGENTS.md`](AGENTS.md)“变更记录”。

## [Unreleased]

### 视觉/音乐/Agent 配置卡在真实桌面窗口可用，主窗口特权命令固定到本进程后端地址（2026-10-13）

- **现象与影响**：主窗口启动后导航到 `http://127.0.0.1:<端口>`，对 Tauri 属于 remote 来源；`voice-vision` / `voice-music` / `voice-agent` 三个 capability 未声明 `remote`，Tauri 只对本地打包页授权，真实 App 中这三张配置卡的命令会被 ACL 拒绝（此前只在假桥页面验证）。另一方面 `default.json` 以 `windows:["*"]` + `127.0.0.1:*` 授权 `get_desktop_session`、`voice_save_settings`、`voice_server_start`、模型密钥保存等命令，命令本身不校验来源：任何本机端口的页面一旦进入桌面窗口，就能取得可批准 R3 的桌面会话，或改写服务端 Python 路径后启动任意程序。
- **根因与修复**：把 `voice_mcp.rs` 已有的来源判定提为共用 `is_trusted_main` / `window_is_trusted_main`（`main` 窗口 + 来源等于本进程后端地址）。`get_desktop_session`、语音中心 8 个改写/日志命令、内置语音 18 个改写及视觉/音乐/Agent 命令在函数开头校验，失败返回 `voice_acl_denied`；三个 capability 补 `remote: http://127.0.0.1:*`，端口由命令内固定。只读状态命令（`voice_status`、`voice_mode_changes`、`voice_builtin_status`）不变，隐私窗口仍可读状态。
- **契约与安全**：命令参数不变（`window` 由 Tauri 注入，前端无改动）；非主窗口或其他来源调用上述命令改为报错。原测试断言这三个 capability “不得有 remote”，把缺陷固化为预期，已替换为更严格的断言（remote 只能是回环通配，且命令必须先校验来源）。默认值与安全门禁不变。
- **验证**：`moss-desktop` 新增 `trusted_main_requires_main_label_and_exact_backend_origin`、`main_window_commands_pin_backend_origin`，桌面 crate 27 项通过；`cargo test --workspace` 716 通过 / 7 忽略，前端 `npm test` 109 通过。**未在真实桌面窗口点击验证**（HW-14），未构建安装包。
- **关联**：`apps/desktop/src-tauri/src/{lib,voice,voice_builtin,voice_mcp}.rs`、`capabilities/voice-{vision,music,agent}.json`、`docs/developer/{api-contracts,safety}.md`。

### 模型、视觉、插件保存不再出现“设置已改、密钥未写”的半写状态（2026-10-13）

- **现象与影响**：密钥存 SQLite `app_secrets`、其余配置存 `settings.json`。插件保存先写设置再逐条写密钥，中途失败会留下“插件已开启但缺密钥”；模型与视觉保存先逐条写密钥再写设置，设置写入失败会留下“密钥已换、设置未变”。插件保存还在校验与写入之间释放数据库锁，并发保存可改掉校验时看到的密钥状态（NV11-4 记录的“跨存储原子性未验”）。
- **根因与修复**：`RuntimeDb::apply_secret_changes` 在一个事务内批量写入/删除并返回原值，`restore_secrets` 逆序还原；桌面 `commit_secrets_then_settings` 先提交密钥事务再写设置，设置失败即还原密钥。三处保存统一改用它，并持锁到提交完成。
- **契约与安全**：接口与错误码不变；无 schema 迁移。还原本身失败时返回合并错误提示重新保存。
- **验证**：`laser-runtime` 新增 `tests/secret_changes.rs` 2 项（先确认缺方法编译失败再实现），`moss-desktop` 新增 `settings_failure_restores_secrets`、`settings_success_keeps_new_secrets`。未模拟磁盘满等真实写入故障。
- **关联**：`crates/laser-runtime/src/db.rs`、`apps/desktop/src-tauri/src/voice_builtin.rs`。

### 安装与卸载前检查本地模型辅助程序是否在运行（2026-10-13）

- **现象与影响**：NSIS 钩子安装前只查 `moss-desktop.exe`、卸载前另查 `moss-host.exe`；带 helper 的专用包里 `moss-voice-local.exe` 运行时文件被占用，覆盖升级或卸载会失败或残留。
- **根因与修复**：安装与卸载前统一检查 `moss-desktop.exe`、`moss-host.exe`、`moss-voice-local.exe`，任一运行即提示退出并取消，不强杀。打包规范中“检测 `moss.lock`”“删除前校验路径以 `\moss` 结尾”的旧描述与实现不符，已改为实际行为（按进程名检测、删除固定路径 `%APPDATA%\moss`）。
- **验证**：未构建安装包、未实际安装（PKG-07 / FINAL-01）。
- **关联**：`apps/desktop/src-tauri/installer/hooks.nsh`、`docs/developer/packaging-install.md`。

### NV11六类供应商契约补齐，缺来源、异常额度与RSS损坏不再伪装成功（2026-10-12）

- **现象与影响**：已有Metaso模块/独立凭据/供应商选择，但任务书仍写未集成；供应商测试仅最小样本。RAG缺来源或返回非白名单数据集仍可能回填；新闻字段类型错误被当默认值，RSS在满足条数后提前返回导致坏尾部未验证，quick-xml GeneralRef被跳过会损坏含`&amp;`的URL；截断未计算JSON转义后长度；Tavily额度及Metaso HTTP200应用错误被泛化为网络/schema问题。
- **根因与修复**：复用已有HTTP/ToolExecutor，不新增生产依赖；HTTP保持3秒连接/10秒总时限/1MiB并显式禁跳转和自动重试，432/433映射不可重试`plugin_quota_exceeded`，IPv6回环/ULA按真实IP解析。Metaso按固定官方SDK普通搜索契约保留（专题ID可选），分类401/403、4002、400/4009、500/-500，不回填errMsg/会话/余额。RAG要求非空document_id/dataset_id且响应数据集在配置白名单；和风校验条件码；NewsNow时间保留数字/字符串类型。RSS完整结构解析、CDATA/预定义/数字引用、非法URL/实体/坏尾部拒绝；模型可选参数类型严格检查，结果按转义长度预算。
- **默认、安全与跨端**：五工具默认关闭，Tavily/Metaso密钥独立、错误不自动切换或重试，真实账号/供应商请求未执行。管理入口仅packaged voice-private，原急停raw路径保持；无数据库迁移或helper协议变更。响应来源缺失/不匹配现在明确失败，新增固定错误码，调用方须按错误处理；Android不启用本地语音server，但照常重新交付构建。新增rcgen/rustls仅dev-dependencies，用于真实本机TLS合成测试，不改变生产信任根或配置。
- **验证**：本机临时TLS通过六家生产adapter，合成请求/响应覆盖成功、缺字段、显式空结果、401/403/429/432/433/500/302、已知/chunked超大、准确1MiB、头/正文超时、无跳转外传与安全错误；RAG来源/Metaso应用码/RSS实体/缓存TTL容量去重/配置变化撤销/原急停保留。实际React+假native验证旧设置/default off/供应商选择/独立Keep-Set-Clear/失败草稿/成功清除密钥草稿/窄屏。全量、定向最终数量和本次三端产物另附完成回执；不能用这些样本证明真实账号、限额或数据质量。
- **关联**：[样本与固定官方依据](crates/moss-voice-server/tests/fixtures/nv11/README.md)、`plugins/{http,weather,search,metaso,rag,news,newsnow,runtime,cache,contract_tests}.rs`、`frontend/tests/voice-plugins.browser.mjs`、API契约及实施TODO。未覆盖跨settings文件/多secret写入的存储故障原子性，NV11-4保留独立故障注入待办，不把前端假桥当真实DB回滚证明。

构建回执见 [build-receipts.md](docs/developer/build-receipts.md)。

### 隔离验收失败不再伪成功，声纹/固件诊断工具与ORT候选源码补齐（2026-10-12续做）

- **现象与根因**：NV13 bwrap诊断原先只打印BLOCKED、可能退出0，自动排期会误判通过；镜像标签可漂移。NV16缺本地标定报告工具；NV17缺可重复的持钥证明/清单签名契约夹具。NV18单个ORT源码tar缺上游声明依赖与gitlink内容，不能证明静态库源码闭环。
- **修复与新增**：NV13核对并使用不可变image ID，增加schema1诊断回执；blocked或仅基本read都失败，production_ready始终false。NV16新增显式consent/WAV/模型hash/协议边界的本地诊断，匿名聚合FAR/FRR/误识别/Wilson上界和15组候选，空指标null，无录音/向量/姓名输出、无自动开启。NV17新增真实Ed25519/SHA的测试用协议夹具，签名清单+单次票据+设备持钥证明；没有生产路由/设备实现。NV18从固定ORT根tar解析41声明，收集35份锁定源码及3个真实gitlink候选，6个主机二进制不冒充源码；流式有界下载、无覆盖、逐文件hash/实际许可复核，仍pending。
- **授权与默认边界**：用户已批准启动Docker Desktop及无账号/无模型/合成测试，并明确暂不连接真实HA。实际内层user namespace仍拒绝，REAL_CLI_SANDBOX_VERIFIED/DisabledRunner不变；未改变Seccomp、特权、AppArmor或内核。Calibration=None、OTA空URL不变；不采集真人音频，不读取私人录音/凭据，不安装、签名、发布或操作设备。原NSIS源码说明更新，新增候选资料不代表实际链接或许可就绪。
- **验证**：实际Linux Codex0.162.1版本/help/外层只读禁网/回收PASS；内层bwrap exit1并准确产出blocked回执，不能记PASS。Rust opt-in Docker探针在显式进程级docker.exe路径后1/1通过；HA合成TLS/服务33项通过。NV16/17 Node测试11项通过（含helper环境只保留Windows必要项，不继承凭据/profile且在模型目录运行），公开模型+实际helper+合成WAV报告运行通过、production_ready=false。ORT38归档431,766,263字节/声明源码无缺失，实际字节审计通过但严格发行仍拒绝对应审核和递归闭环两类；匿名release/attestation未取得构建证明。本次全量、各端与专用内部包回执如下。
- **关联**：`test-nv13-{linux-codex-image,codex-bwrap-sandbox}.ps1`、`calibrate-nv16-speaker.mjs`、`scripts/tests/nv16-calibration.test.mjs`、`scripts/tests/nv17-download-protocol.test.mjs`、[可信下载候选协议](docs/integration/nv17-authenticated-download-contract.md)、`collect-nv18-ort-dependencies.mjs`及NV18只读审计。

构建回执见 [build-receipts.md](docs/developer/build-receipts.md)。

### 当前句身份绑定后隔离个人历史和摘要，撤销与取消不能留下旧结果（2026-10-12）

- **现象与影响**：NV16三段登记已有真实helper/SQLite接线，但会话仍只用设备历史；先前合成按人夹具未能证明甲乙/未知对话隔离。异步识别在首轮运行前取消可能留下设备占用，旧身份在撤销后重新允许时也必须失效；摘要多窗口冲突可能被轮询清掉错误并丢失版本语义。
- **根因与修复**：新增`person_context.rs`及Conversation/Session、桌面`DesktopIdentity/DbPersonStore`接线，快照绑定人revision/目录代次/设备/会话/授权代次/本地epoch；历史按设备+会话+人隔离，未知者不读写个人历史或回退设备摘要。top1/margin/192维有限值校验；ASR并行、全局2/设备1、无识别队列、1.5秒上限，停止/退出先返回。取消清理guard在spawn之前构造；桌面阻塞worker许可持有到真实收尾，避免异步取消释放后无界排队。
- **个人摘要契约**：断线总结队列16/并发2/LLM30秒/最多1500字；模型调用前与CAS写回前复核身份及设备授权，删除/撤销/重新授权/目录变化拒绝旧结果。敏感内容、工具调用、空回复、错误均保留旧摘要。私有`voice_speaker_manage`新增`save_memory`和`personal_mode`，只返回元数据/文字、不序列化向量；本机摘要编辑可离线使用，草稿绑定初始revision，409错误保留，提供明确丢弃并加载最新值操作。
- **默认值、安全和升级**：新增`builtin_person_memory_enabled`默认false，旧配置不自动开启，普通设置入口不能修改。生产`Calibration=None`，缺真人标定时不能开启识别/按人模式；设备记忆开关不变，不迁移共享摘要，不扩展ToolContext或设备审批权限。云模型会收到筛选后的总结文字，原始PCM只在内存。schema9/协议4不变；Android不编译私有识别入口。可选仅登记者加工限制/外部识别/WAV导入未实施。
- **NV15协调器验证**：仅cfg(test)注入内存假HA，真实临时RuntimeDb/ApiService校验prepare/approve/单次消费、错误digest/取消/撤销0发送和失败Unknown不重发，7项通过。trait transport复用既有执行器，生产控制门禁仍false，不启用POST。
- **NV18许可收集修正**：之前按文件名扫描漏掉r-efi的AUTHORS许可正文，已从5.3.0/6.0.0锁定checksum源码提取完整MIT grant/版权原文；146归档/279文本，缺失列表为空。审计逐项核对Cargo.lock/源包/文本字节和文件类型；仍拒绝原生对应审核、ORT传递依赖闭环两类阻塞，不伪造审核人或外部分发许可。
- **验证**：workspace698 PASS/7 ignored，前端109 PASS，FakeGrbl32 PASS（新release moss-host）；实际React fake native验证显式登记、二元设备/会话、取消、标定禁用、摘要冲突保留/重新加载/保存及窄屏。按人/假WebSocket测试覆盖甲乙未知、共享摘要不回退、旧会话/重新授权/删除/排队撤销、首轮前取消和停止不等待；临时SQLite与设置兼容/普通窗口绕过拒绝通过。最终跨端和专用内部包回执见下；真人准确率、实际HA/CLI/OTA、安装发布仍partial/unverified。
- **关联**：[声纹说明](docs/integration/nv16-speaker-enrollment.md)、实施TODO、`person_context.rs`、`conversation.rs`、`session.rs`、`voice_builtin.rs`、`VoiceSpeakerCard.jsx`、`home_control.rs`及NV18收集/审计脚本。

构建回执见 [build-receipts.md](docs/developer/build-receipts.md)。

### 声纹登记接入私有窗口、固件状态独立呈现与发行资料收集（本轮续做）

- **现象与影响**：NV16仅有合成匹配器和表结构，helper声纹API可能误用共享语音进程；按路径hash后原生重新打开存在替换窗口。NV15控制发送已硬关闭但仍可能报告可控制/创建不可执行审批。NV17下载状态未接私有UI；NV18仅有SPDX元数据，缺实际依赖源码/许可文件。
- **根因与修复**：固定CampPlus文件、SHA/28,281,138字节/16kHz/192维，协议4严格验证代次及f32le；新增私有 `SpeakerHelper`专用进程及Windows文件/祖先目录禁止写删共享的加载锁。登记由固定私有窗口明确确认、当前设备会话和实时permit绑定，90秒/3段/2～10秒/无队列、1.5秒超时；24/48kHz重采样、质量和完全重复录音拒绝、旧代次/取消/撤销防晚到写入。SQLite归一化/一致性/最多64人/重复姓名/元数据不带向量/CAS删除个人摘要同事务。无效删除不取消他人的登记，超时收尾期间阻止新登记。
- **默认值与边界**：声纹识别、个人摘要注入和仅登记者加工限制仍关闭，登记不能授权设备。Windows以外声纹安全加载拒绝；Android无私有声纹/固件命令。已有设备记忆开关不变，原始登记音频不落盘。NV15授权、可控制、可批准、生成审批和POST统一硬关闭；取消/凭据变更取得发送锁后才改变状态，保留正式网络门禁。
- **固件与发行资料**：私有只读交付记录区分下载和设备上报；启动把未完成下载转unknown，不自动重试或声称刷写。OTA维持空下载URL。固定收集10份原生候选源码归档、13库映射/实际许可；146份Cargo.lock锁定registry源码和277份许可文本，两个r-efi版本缺完整许可文本。原生对应审核、ORT递归依赖及传递许可闭环保持阻塞，review_status=pending，禁止自动伪造批准。专用内部NSIS收集源码、许可和证据清单；Windows检测改为实际RuntimeInformation，避免OS环境变量缺失误判。
- **验证**：合成登记3项、协议边界/专用helper超时、临时SQLite、私有ACL；公开真实模型+合成PCM实际helper输出192维，禁止写/替换负测通过。实际React确认/二元会话绑定/取消通过；workspace、109项前端、FakeGrbl32项及当次交付构建结果在最终回执补齐。发现并修复取消配额测试主动抢占导致的无限等待，改为oneshot确认已获得配额；登记测试等待可观察完成状态替代固定20ms睡眠。实际硬件、真人准确率、安装、真实HA/CLI/OTA、外部分发均unverified/partial。
- **关联**：`speaker_enrollment.rs`、`local.rs::SpeakerHelper`、`RuntimeDb::enroll_voice_speaker/list_firmware_deliveries`、`VoiceSpeakerCard/VoiceFirmwareCard`、`home_control.rs`、`collect-nv18-*-sources`、[NV16登记说明](docs/integration/nv16-speaker-enrollment.md)、实施TODO与NV18许可审计。

构建回执见 [build-receipts.md](docs/developer/build-receipts.md)。

### NV15 控制硬关闭、GET/POST 网络策略一致与 NV14 音乐安全修补（2026-10-11）

- **问题**：正式 HomeControl 审批协调器存在可能接入真实 HTTPS POST 的路径，而 GET 已固定 DNS/IP 且 POST 未共享该检查；音乐允许根目录内部的敏感子目录被索引；同名曲目仅用同一标题提示无法区分；S1-09 Android Origin 实机检查误勾选完成。
- **修复**：生产 `HomeControlCoordinator::approve` 在访问网络或消费审批前直接返回 `home_control_disabled`，正式 HTTPS 传输 `submit_once` 非测试编译下仍拒绝。生产控制传输构造与只读发现共用 `home_readonly::pinned_client`，拒绝非私有/混合 DNS 地址、代理和跳转并固定 TLS 目标；未启用真实 POST。NV14 音乐索引跳过敏感子目录并在 resolve 时再次校验，同名候选提供编号、标题和索引 ID，无法明确选择时不播放。S1-09 标为进行中、NV15 不宣称正式控制完成。
- **验证**：`cargo check -p laser-host -p moss-voice-server -p moss-voice`、`cargo test --workspace` exit 0；前端 `npm test` 108/108 PASS、`npm run build` PASS，FakeGrbl E2E 退出码 0；最后一次 UI 提示修改后再次前端测试/构建并重建两端。Windows Release `target/release/moss-desktop.exe` 58,317,824 B，SHA-256 `7fcc304bba4a3c03854cb1bade7810714708488ceb5dff44df08bb55f9761dfc`（2026-10-11 13:21:27 -04:00）；普通 NSIS `MOSS_0.1.0_x64-setup.exe` 18,544,625 B，SHA-256 `c3c73d2295381046d0fb49d9609a524ea9772f4b4556ac835779820aee0399dd`；Android `app-universal-release.apk` 52,508,238 B，SHA-256 `1f31f36acd235370a94cbc15608163a192ff7cb7371a05b2997405d8ca3115f1`（13:23:05 -04:00）。只证明构建，不证明真实 HA、Android Origin 或安装 GUI；这些保持 partial/unverified。生产 POST 门禁不得为测试放宽；普通 NSIS 不代表 NV18 helper 发行合规。
- **关联**：`crates/laser-host/src/{home_control.rs,voice_home/https_transport.rs}`、`crates/moss-voice/src/home_readonly.rs`、`crates/moss-voice-server/src/music/{index,tools}.rs`、`docs/integration/moss-fusion-implementation-todo.md`。

### NV15 凭据备份、并发撤销、独立审批及模拟结果闭环（2026-10-11）

- **可移植脱敏备份**：新增 `moss-home-profile-v1` 固定 64KiB JSON 格式，只允许全部停用的实体清单和迁移后重新输入凭据的标记（HA HTTPS origin、media library IDs 也全部清除）。明确禁止 app_secrets、DPAPI 密文、令牌、审批及其他普通 voice 配置随备份导出。导入时默认禁用全部实体并强制 `ask`；私有桌面窗口新增导出、粘贴恢复和二次确认。恢复时先撤销原 token 和全部家居授权/旧审批，再 CAS 保存白名单；写配置失败也不恢复旧凭据。历史 SQLite、WAL、外部备份可能仍有明文，程序不自动删除用户备份；疑似泄漏应主动轮换 HA token。
- **原子撤销与并发 CAS**：数据库 HA token 更换/明文迁移采用 SQLite `BEGIN IMMEDIATE`，多连接同时写入仅一方成功；token 更换和清除在同一事务删除 `voice.homeassistant.voice_read_consent`、`voice.homeassistant.voice_control_consent`，过期 HomeControl pending/approved 记录。桌面令牌/白名单/恢复使用 send fence 先撤销 local pending 再串行化发送边界；`discover_guarded` 两次 GET/解析结果之后重新核验白名单、令牌版本，HomeControl 首次读取目录改用同一 guard。**已经发出的物理请求无法收回**，生产设备响应仍未验收。
- **独立审批和模拟结果**：HomeControl 与 AgentRun/ExternalTool/激光批准独立，关联 approval_id、设备、fingerprint、digest 和一次性状态机。最终 Completed 或 result_unknown 连同固定码审计写入一个 SQLite 事务；审计失败回滚终态，不会把结果虚报为成功；已经消费的请求不能重发，重启后以 Unknown 收口。合成两张不同审批、跨设备/跨摘要防混用、结果未知、迟到成功与审计失败回滚的专项通过。不存在真实 HA POST/真实密钥调用。
- **测试边界**：Portable backup/恶意字段和旧版清洗、SQLite 旧令牌迁移/外来 DPAPI/并发/CAS 专项 **6/6**，HomeControl mock 结果 **6/6**，语音家居安全专项 **19/19**，额外撤销发送同步 **3/3**；桌面 Rust 检查及前端测试/构建 PASS，FakeGrbl E2E **32/32 PASS**。本轮 `cargo test --workspace` 的远程任务状态在其他 Cargo 构建并发下丢失，**不得将全量 Rust 或 Windows/Android release 标为本轮已验证**；需要待 Cargo 缓存争用解除后重新验证。真实 HA 连接、历史备份盘点、WebView2 人工确认与实际设备动作留 FINAL，NV15 仍 partial。
- **跨账户 DPAPI 和真实撤销发送线性化补测**：识别外来 DPAPI 密文时允许 RuntimeDb 以凭据不可用状态启动，但令牌读取和控制始终拒绝；仅在 voice-private 明确确认的恢复流程中使用当前单调版本 CAS 清除旧密文与 grants。每条 HomeControl 拥有独立 active 撤销位；令牌更换、白名单保存、恢复及专用审批撤销共用发送锁，阻止撤销落在最后检查与 POST 之间。新增合成 DB 外来密文恢复/错误版本拒绝、独立请求撤销并发及模拟执行后不确认/不重放测试。实际外部 POST 无法撤回，状态未知只记录 result_unknown，不虚构安全已停止。专项测试通过，完整三端回归仍待最新产物确认。

### NV15-1 无字段名 Authorization 日志泄漏防护（2026-10-11）

- **现象与根因**：原日志脱敏仅在 `Authorization:` 等字段后识别 Bearer/Basic 值；历史外部日志中的裸 `Bearer <凭据>` 或 `Basic <凭据>` 会通过读取与日志导出路径暴露。
- **修复**：`moss_voice::redact::redact_line` 两种 Unicode 路径均增加独立身份前缀识别和有界值遮盖；`RotatingLog::tail` 本来就重脱敏旧日志，本轮新增合成旧日志只读回归，不删除、覆盖或迁移原日志。用于日志显示/诊断回看，不对历史原始备份自动处理。
- **验证**：使用 Windows 本机 `rustc --test` 独立执行 `redact.rs` **3/3 PASS**，增加 `logfile.rs` 旧日志 tail 回归（待 cargo 锁解除执行）；不会把独立测试冒充 Rust workspace 全绿。不会触碰真实私有日志。
- **关联**：`crates/moss-voice/src/redact.rs`、`crates/moss-voice/src/logfile.rs`。

### NV15-1 历史备份只读桌面审计入口（2026-10-11）

- **影响与根因**：之前仅有 Rust 64MiB 旧备份风险分类，用户在安全窗口无法主动选文件审计；直接开放任意路径将形成敏感文件读取入口。
- **修复**：仅安装包内 `voice-private.html` 可调用新 `voice_home_backup_audit`；由 Rust 主进程弹出系统文件选择器，前端不上传路径、原文或令牌。`audit_selected_file` 拒绝相对路径、`..`、符号链接、非普通文件、空文件和超过 64MiB 文件，Windows 同时拒绝非本地磁盘前缀及任意路径组件的重解析点。只返回固定风险枚举、读取字节数和 `safe_to_share=false`；未知仍按敏感处理；不解密、不恢复、不删除、无后台扫描。日志错误使用固定文字，不含路径或字节。
- **验证**：新增相对路径、父路径与 Unix 符号链接反向测试；本轮前端 Node 测试 107/107 PASS；全量 Rust、E2E 和跨端构建以随后本轮完成回执为准。Windows 实际文件选择 GUI 与真实历史备份内容尚未验收，不得将关键词分类当作无泄漏证明。
- **关联**：`crates/moss-voice/src/home_backup_audit.rs`、`apps/desktop/src-tauri/{src/voice_builtin.rs,src/lib.rs,build.rs,capabilities/voice-home.json}`、`frontend/src/{voiceApi.js,components/VoiceHomeCard.jsx}`。

### NV15-1 受限脱敏备份、恢复与历史凭据风险检查（2026-10-11）

- **风险**：直接打包 SQLite/WAL/旧设置可能包含旧版 Home Assistant 明文令牌、DPAPI 密文和批准记录；不能将简单复制数据库视作安全可移植备份。未知历史文件不能通过未检测到关键词就宣布安全。
- **现状核验**：专用 `export_home_portable` / `validate_home_portable` 与打包 `voice-private` 的 `voice_home_backup_export`/`restore` 已在当前代码存在：只导出 Home 白名单受限 JSON，不含全局 voice 配置、令牌、批准或授权；恢复强制失效旧 Token/授权并关闭所有实体，重新输入令牌后才能使用。旧数据不由程序自动删除。已有合成机密和嵌套恶意字段测试。
- **新增**：`moss_voice::home_backup_audit::audit_selected_file` 可供后续界面选定单个文件进行离线风险分类：仅只读64MiB以内非符号链接普通文件；固定缓冲区跨边界识别少量旧凭据字段、DPAPI 标记；返回风险类别和字节数，绝不返回文件内容。未识别的文件一律标记 `UnknownTreatAsSensitive`，**不能证明其中无凭据**。该审计核心已接入 voice-private 系统文件选择器（限定绝对路径、无父目录跳转、Windows 重解析点拒绝），仅在用户主动选择时读取；本轮只对临时合成样本执行，没有扫描用户真实备份。旧 WAL 和外部快照不会自动销毁，疑似旧版明文应建议轮换 HA Token。
- **验证**：新 `nv15_legacy_file_audit_*` 专项通过；Rust workspace、前端/FakeGrbl、Windows/Android 最新重建结果另见本轮 Goal。真实 HA、旧备份处理、恢复安装验证尚需授权。
- **关联**：`crates/moss-voice/src/{home_backup_audit.rs,lib.rs,settings.rs}`、`apps/desktop/src-tauri/src/voice_builtin.rs`。


### NV15-1 凭据撤销请求边界和合成 SQLite 备份泄漏测试（2026-10-11）

- **影响/根因**：旧只读请求曾在两次 GET 后才复核凭据；撤销后还可能继续发送第二条 GET。旧 DPAPI 加密仅有 SQLite 内部值断言，尚缺 WAL/SHM 合成检查。
- **修复**：`discover_guarded` 在 DNS 前、状态 GET 前、服务 GET 前和返回前检查白名单及 token 修订；桌面只读和语音路径都复核当前授权；撤销时不再发送新的 GET，也不返回旧结果。**已发出的请求不能撤回**，正式网络断线/强取消还需验收。
- **备份边界**：仅用虚构 token 进行 SQLite 双连接 CAS 和主库/WAL/SHM 明文检查；旧 WAL、数据库复制、外部备份、快照依然可能含历史明文。现阶段**禁止将原始 SQLite/WAL/SHM 视为安全可移植 HA 凭据备份**，跨机器恢复须重新输入 Token；用户应审计旧备份并轮换可能泄漏的旧 Token。不擅自删除备份。自动脱敏备份、恢复和凭据泄漏完整审计仍未完成。
- **验证**：`nv15_revocation_guard_is_fail_closed_before_dns_or_requests`、`nv15_backup_does_not_persist_plaintext_and_two_connections_cannot_reuse_old_revision` 专项通过；完整构建和回归见本轮 Goal 记录。
- **关联**：`crates/moss-voice/src/home_readonly.rs`、`apps/desktop/src-tauri/src/voice_builtin.rs`、`crates/laser-runtime/tests/nv16_nv17_schema.rs`。


### NV15-1 设备不可用不再显示过期服务和属性（2026-10-11）

- **现象与影响**：HA 已上报实体 `unavailable`/`unknown` 时，只读发现仍列出该域名支持的 service；定时同步断线仅清空 actions，却保留亮度、温度及旧时间戳，可能误导使用者将旧状态当作实时状态。
- **根因**：`parse_snapshots` 检查 service 是否在域可用，而未检查当前实体是否在线；React 断线只更新了 state/actions。
- **修复**：发现实体为 unavailable/unknown 时无可用 service，清空对应设备 attributes；前端同步失败同时清除缓存的敏感属性与时间戳；服务重新上线时仅使用新返回的明确授权实体快照，不自动新增白名单。
- **验证**：moss-voice `nv15_` 专项已通过，新增离线/重新上线/非白名单实体回归；完整 workspace/前端/FakeGrbl/Windows/Android 以最终构建回执为准。真实 HA 认证、备份历史 WAL 安全销毁以及 Windows WebView2 实际操作仍未验收。
- **关联**：`crates/moss-voice/src/home_readonly.rs`, `frontend/src/components/VoiceHomeCard.jsx`。


### NV15-1 HA 只读 HTTPS 连接的 DNS 固定与敏感地址拒绝（2026-10-11）

- **风险与根因**：HA 只读发现原来依赖 HTTP 客户端隐式解析，`no_proxy` 和禁止跳转不足以限制 DNS 地址变化及公网/链路本地元数据地址；可能把 DPAPI 解密的 Bearer 令牌发送到错误目标。
- **修复**：`moss-voice::home_readonly::pinned_client` 在连接前仅解析一次已配置 origin，所有解析地址必须属于 RFC1918、回环、Tailscale CGNAT 或 IPv6 ULA/回环；混入公网/链路本地/未指定/组播地址一律拒绝；通过 reqwest `ClientBuilder::resolve` 固定该批地址，仍使用原始 HTTPS 主机名和正常 TLS 证书验证，不跟随重定向、无代理、限时限字节。不会访问真实 HA 或发送控制 POST。**注意**：公网域名直连 HA 当前按安全边界拒绝，后续需明确可信地址策略，不能为了兼容而取消校验。
- **验证**：增加 NV15 地址分类单测（私有/Tailscale 允许、公网/云元数据拒绝），`cargo test -p moss-voice nv15_ --lib` 初步通过，完整回归和双端构建以最终验证结果为准。DNS 解析结果本身在此无独立审计；真实 HA TLS/Token/Windows WebView2 仍留待用户授权验收。
- **关联**：`crates/moss-voice/src/home_readonly.rs`。


### UI-03 Geist 字体随包自托管，离线显示不再依赖字体安装（2026-10-11）

- **现象与影响**：样式指定 Geist/Geist Mono，但交付包没有字体，目标电脑始终回退到系统字体，界面字宽与预期不一致。
- **根因**：仅声明 font-family，未提供本地字体及 @font-face。
- **修复**：从官方 vercel/geist-font 固定提交收集两份未修改的可变 WOFF2 与完整 OFL-1.1，存入 frontend/public/fonts；两项 @font-face 使用同源地址和 swap，Vite/HTTP 宿主/Tauri 共用字体资源。保留中文系统字体回退，不增加外网访问、权限或配置迁移。
- **验证**：来源、文件大小与 SHA-256 已核对；前端 107/107 通过。渲染、回环 E2E 和三端正式构建继续执行，结果随后补入本条；真实显卡性能仍留 HW-06。
- **关联**：frontend/src/styles.css、frontend/public/fonts/{README.md,OFL.txt}、THIRD_PARTY_NOTICES.md；任务 UI-03。

### NV15 HomeControl 私有审批收件箱及不可重放撤销（2026-10-11）

- **现象/风险**：原 HomeControl 审批数据不能在安全窗口独立查看或撤销；普通审批端点故意拒绝其批准，错误复用会穿透激光审批安全边界。
- **修复**：`ApprovalService::cancel_home_control` 仅接受真实 HomeControl 类型、正确 digest、未过期且为 pending/approved 的记录；SQLite 条件事务将 pending→rejected、approved→expired，拒绝 executed/failed/unknown 重放，保留历史审计。桌面新增双重 `voice-private` ACL 的 `voice_home_approvals` / `voice_home_approval_cancel`，只显示经过过滤的实体、固定服务、无密钥参数、摘要与状态，不回传连接配置和原始请求。`VoiceHomeApprovalCard` 展示历史、状态和撤销按钮；批准执行因正式控制链未通过安全验收始终锁定，任何操作都不发送真实 Home Assistant POST。
- **验证**：新增 HomeControl 撤销专项覆盖错误 digest、待批撤销、已批失效、已执行拒绝重放；最终 Rust `cargo test --workspace` **644/644 PASS**，前端 `npm test` **107/107 PASS**、Vite build PASS、FakeGrbl E2E **32/32 PASS**。Windows 正式 `cargo tauri build` 经消除并行 `frontend/dist` 构建争用后 exit0：`target/release/moss-desktop.exe` 57,534,976 bytes SHA256 `f3961f6a8bee4b8efa631437afb6492c6e2f9e4ac8149e7a1fc1724f92fa94b8`、普通 NSIS 18,170,964 bytes SHA256 `bfcbdf462a9991a5d429c0f9c8b6a96b430d8cbf869e6c4d2b8c7efd649f5842`。Android `cargo tauri android build --target aarch64 --apk` exit0：APK 52,085,206 bytes SHA256 `0c7090ba91244c7599986ef57074ce187cf4a8ed1890e685116ed8d7803e1e50`。普通 NSIS 不是 NV18 合规 helper 专用安装包。实机、真实家居服务、WebView2 GUI 交互和实际批准执行均未验收，NV15 继续 partial。
- **关联**：`crates/laser-api/src/approval.rs`、`apps/desktop/src-tauri/{src/voice_builtin.rs,src/lib.rs,build.rs,capabilities/voice-home.json,Cargo.toml}`、`frontend/src/{components/VoiceHomeApprovalCard.jsx,voicePrivate.jsx,voiceApi.js}`。


### NV15 语音只读授权实际双端构建与软件回归（2026-10-11）

- `moss-voice-server::home_tools` 三个保留名称的动态能力；HomeControl 控制硬门禁 `control=false`，只有本地安全窗口单独授权、且白名单/DPAPI token 修订仍匹配和 voice session 未撤销时才显示和执行 `hass_get_state`。`hass_set_state`、`hass_play_music` 在生产不可使用。
- **本轮完成的验证**：`cargo test --workspace` **638/638 PASS**，前端 `npm test` **107/107 PASS**，Vite build exit0，FakeGrbl E2E **32/32 PASS**；Windows `cargo tauri build` exit0，`target/release/moss-desktop.exe` 57,383,936 字节 SHA-256 `3fc4463289359919d6ef020db021b4d37d57dca75d70f693f5774d784b482fe7`，普通 NSIS 18,151,741 字节 SHA-256 `d78293365a098fc2add88f5eaa8015e56dc66779ab3ba7a45f237dcebe2712c1`；Android `cargo tauri android build --target aarch64 --apk` exit0，APK 52,004,918 字节 SHA-256 `9910b7f8571855761efec5c4ae3f7bf99855f2d9169c80a792c18e42d9b71c20`。仅为离线软件及构建证据，**真实家居/本地安全 WebView2/完整生产审批 UI、可信 DNS/TLS 环境验证和设备控制未验收**。未提交代码，未接设备/账号。


### NV15 撤销竞态最终全量回归与双端构建（2026-10-11）

- **软件验收**：本轮 `cargo test -p laser-host voice_home --lib` **31/31 PASS**；`cargo test --workspace` **643/643 PASS**；前端 `npm test` 和 `npm run build` 退出码 0；FakeGrbl 模拟器 E2E **32/32 PASS**，临时宿主进程正常回收。只使用软件合成 HA 证书、假令牌和本机回环模拟控制，不访问真实设备。
- **Windows 本轮新产物**：`cargo tauri build` 退出码 0，`target/release/moss-desktop.exe` **57,383,936 B**，SHA-256 `c2be1f6fd0290324fbde321820a1255754a79c63fb59bed6f39bfd0ad04f5387`；普通 NSIS `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` **18,157,520 B**，SHA-256 `d4bd526cbe2ca28f0617513da8e0d3102a6c9cb9ab7416f44f648e3c409bcadc`。两项均检查在本次构建启动之后更新。**普通 NSIS 不代表 NV18 带语音 GPL helper 的专用安装包**。
- **Android 本轮新产物**：`cargo tauri android build --target aarch64 --apk` 退出码 0，`apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` **52,004,982 B**，SHA-256 `d984ef6ff2764435254fd044bfb60564c0d81f962d24aa02cb0b1a4592b5d54d`；APK 中 arm64 Rust `libmoss_desktop_lib.so` SHA-256 与本轮编译生成的 `.so` 完全一致，均为 `91c0470b075f8674647679e430c0c0ad635173a7a6645db8061324105142ab3b`。
- **交付边界**：没有安装客户端、读取用户真实 HA 账号、发送真实设备控制、连接激光机或提交/推送代码。`HomeTools` 仍为 `control=false`，生产 HomeControl 执行器保持未注册；真实 HA TLS/DNS/凭据有效性、桌面 WebView2 审批交互与逐实体真实动作还需另行人工安全验收。此前 CHANGELOG 中各阶段安装包哈希为历史版本，不代表当前磁盘产物。
### NV15 修复家居状态观察撤销竞态与单次 HTTPS 读取超时（2026-10-11）

- **现象/影响**：HomeControl 已发送后读取状态期间撤销设备许可，旧状态查询恰巧返回期望状态时，原逻辑可能仍将动作记录为已确认成功；另外共用 7 秒 HTTP 客户端超时，可能超过总观察 3 秒预算。
- **根因与修复**：`voice_home/approved.rs` 原先只在 GET 前检查许可，现每次读取结束后再次核验实时许可与总截止时间，撤销或超时只记录 `result_unknown` 且已消费审批不可重放。为每次观察传入剩余时间及 900ms 上限；`https_transport.rs` 的 GET 设置单次完整 TLS/HTTP 读取超时，保持 HTTPS-only、禁代理、禁重定向，不改变至多一次 POST 门禁。
- **验证**：新增 `voice_home/https_transport/tls_fixture.rs`，只在临时 loopback、自签证书、合成假 token 上验证目标路径/实体、证书主机名、302/HTML/大响应与慢响应拒绝、服务 changed-states 不等同目标执行证明；状态查询中撤销负向测试验证不误报成功、不重发。Home 专项 **31/31 PASS**；真实 Home Assistant、硬件与用户凭据未访问，生产家居动作禁用。全量回归及交付构建另记录。
- **关联**：`crates/laser-host/src/voice_home/{approved.rs,https_transport.rs,https_transport/tls_fixture.rs}`。

### NV15 语音 Home Assistant 工具注册与显式只读授权（2026-10-11）

- **现象/影响**：Home Assistant 只读同步以前只可从设置页主动发起，语音工具尚未注册；直接将控制名称交给通用 MCP 会混用授权，也可能让模型提供恶意 URL/参数。
- **根因**：语音工具编排原仅包括 HostTools + InfoTools，缺 Home 专用保留名称、按实时家居白名单检查的路由与显式声控授权状态。
- **修复**：新增 `moss-voice-server::home_tools`，为 `hass_get_state`、`hass_set_state`、`hass_play_music` 保留不可被第三方覆盖的名称和拒绝任意 URL/模板/PIN/未定义字段的参数 schema；内置语音组合工具改为 `HomeTools(CombinedTools)`。默认 `read=false,control=false`。只有在本地可信 voice-private 窗口通过 `voice_home_voice_consent` 明确授权、令牌和白名单版本完全一致且实际内置语音 session 仍允许时，才向模型开放 `hass_get_state`。可信桌面后端每次查当前 SQLite DPAPI 凭据、当前白名单/版本，只返回被勾选的精确实体状态和安全属性；网络返回后复核许可、配置与令牌 revision，改变即拒绝。独立只读开关保存在 `app_secrets` 的修订指纹中，旧权限不会因白名单/令牌更新而继续有效。危险工具仍强制 `control=false`，不能产生真实设备 POST，也不能借用激光或第三方工具免批准策略。
- **验证**：HomeTools 独立 Rust 单测验证 disabled 默认、保留名称/不转发、恶意 JSON、会话撤销、只读响应；桌面 Rust lib/完整 workspace、前端单测/build、FakeGrbl E2E 和 Windows/Android 发布构建须看最新本轮记录。没有登录/发送用户真实家居命令。未完成 HomeControl 生产审批 UI、真实 HA DNS/TLS/认证兼容及 POST fake-to-production 绑定，NV15 继续 partial。
- **关联**：`crates/moss-voice-server/src/home_tools.rs`、`apps/desktop/src-tauri/{src/voice_builtin.rs,src/lib.rs,build.rs,capabilities/voice-home.json}`、`frontend/src/{voiceApi.js,components/VoiceHomeCard.jsx}`。


### NV15 HomeControl 一次消费、仅受信 HTTPS 发送器及专用本地窗口（2026-10-11）

- **原因/风险**：原生产 HA 白名单只有只读发现，HomeControl 持久审批与控制发送之间缺少统一一次性状态机；另外回环 HTTP 主窗口没有私有 Tauri 命令 ACL，不能通过开放 remote 权限解决。
- **修复**：将四项 Home 状态、设置、令牌保存、只读发现操作统一迁入已有 `voice-private` 打包窗口，并在每个 Tauri handler 再次严格校验窗口标签和打包资产来源，远程主窗口零敏感权限。前端新增用户显式开启的 15 秒只读状态刷新，超时/401 后提示不可用且不自动授权任何新实体，配置/密钥变化中止刷新。`laser-host::voice_home::approved` 将持久 HomeControl 审批、当前许可/计划/冻结参数校验、SQLite 单次消费/Running 审计和终态 Completed/Unknown/Cancelled 连通为可复用内核；真正发送前再次校验，单个控制最多一次提交，之后仅限目标实体三次只读观测。`https_transport` 单独实现固定白名单路径的 HTTPS 受限请求，TLS/无代理/无跳转/固定 JSON 与大小限额；**尚未注册到生产语音工具，也没有调用真实家居或开放未经授权的控制**。
- **验证**：Home `voice_home` 专项 **26/26 PASS**、独立凭据与 SQLite 集成 **5/5 PASS**，最终 Rust `cargo test --workspace` **636/636 PASS**、前端 `npm test` **107/107 PASS**、前端 Vite build exit0、FakeGrbl E2E **32/32 PASS**。Windows `cargo tauri build` exit0：`target/release/moss-desktop.exe` 57,364,992 B SHA256 `d277e4288849335867362d75f4e0b2565a053e8b9ef4000783da2296ec97ee6e`；常规 NSIS 18,122,094 B SHA256 `ab5e314a3367da10b1000ce87f2133331627d54d324255a8eafc37ee87fb2840`；Android `cargo tauri android build --target aarch64 --apk` exit0：APK 51,979,542 B SHA256 `816d91e612f5147246e628377dd7daaad333caf82799391926fa56de3ecf6180`。**常规 NSIS 不代表 NV18 带 GPL helper 的合规安装包**。真实网络 DNS/证书策略、设备操作、语音控制正式注册、完整审批 UI/控制闭环及安全窗口实际 WebView2 交互尚未验收；NV15 保持 partial。
- **关联**：`voice_home/{approved.rs,https_transport.rs}`, `voice_home.rs`, `VoiceHomeCard.jsx`, `voicePrivate.jsx`, `voice_builtin.rs`, `voice-home.json`, `voicePrivacyAcl.test.mjs`。


### NV15 修复 Home Assistant 长期令牌明文数据库存储（2026-10-11）

- **现象/影响**：旧实现将 HA token 原样保存至 `app_secrets.value`；SQLite 本体/WAL/备份文件可能泄漏长期凭据。普通设置不会回显 token，但独立表不代表静态加密。
- **根因**：`RuntimeDb::update_home_token_if_revision` 仅做 revision CAS，没有调用 Windows 用户凭据保护接口；旧版本已经可能写入明文。桌面只读发现用公共 `get_secret` 原样读取。
- **修复**：新模块 `laser-runtime::home_secret` 在 Windows 调用 CryptProtectData/CryptUnprotectData（用户范围 DPAPI、专用 optional entropy、禁止交互 UI），在 SQLite 中只保存 `dpapi-user-v1:` 加密 envelope。`home_token_for_connection` 只在可信后端内部解密，桌面状态/普通配置不提供明文。数据库打开时对旧明文尝试一次性事务迁移并递增 credential revision、使 pending/approved HomeControl 过期；无法加密/解密、旧数据损坏或非 Windows 后端一律 fail-closed。SQLite 启用 `secure_delete=ON` 降低删除残留；**既有 WAL/外部备份或已导出的旧明文无法由此保证消除，必须额外审计/安全销毁并考虑轮换令牌**。不实际读取或更换用户令牌。
- **验证**：Windows DPAPI 加密/篡改 Rust 单测、旧明文迁移、版本撤销、加密存储/只在受信调用端解密的 SQLite 专项已通过；运行时 lib 39/39，sqlite 集成 5/5。Rust workspace、前端、FakeGrbl、双端发布构建另据本轮最新结果报告。没有启用 HA 控制 POST；HomeControl 生产执行/审核仍未实现。
- **关联**：`crates/laser-runtime/src/{home_secret.rs,db.rs,lib.rs}`、`crates/laser-runtime/tests/nv16_nv17_schema.rs`、`apps/desktop/src-tauri/src/voice_builtin.rs`。


### 修复语音中心插件状态与私人记忆的 Tauri ACL 拒绝（2026-10-11）

- **现象/影响**：Windows App 实际语音中心显示正常，但 `voice_plugins_status` / `voice_memory_list` 返回 `not allowed by ACL`，插件状态与私人记忆编辑器无法加载；这与已结项的前端资源缺失/黑屏问题相互独立。
- **根因**：`lib.rs` 启动后把主窗口导航到 `http://127.0.0.1:<实际端口>/`，而敏感 capability 原只允许来自打包资产的本地 `main` 窗口，未开启 remote 授权。因此 remote 的 `main` 不能调用私有命令，单纯添加宽泛 loopback remote 规则会泄露敏感权限。
- **修复**：新增唯一 `voice-private` 打包页面与受信桌面子窗口；远程主窗口只得到不返回敏感内容的 `open_voice_private` 权限，并以主窗口标签及真实后端 origin 验证来源。两份原敏感 capability 只授权 `voice-private`，仍禁止 remote、未加入 default 通配 scope。六项敏感读写命令加入 Rust 本地 window label + Tauri App URL 二次检查；关闭子窗口直接销毁，主窗口导航和激光关闭拦截不变。Vite 多入口页面仅呈现插件/记忆卡片，无真实密钥回显；Android 仍拒绝。
- **验证**：新增 Rust 窗口来源/ACL 正反向测试、前端 `voicePrivacyAcl.test.mjs` **3/3**；桌面 crate **23/23**、完整 Rust workspace **630/630**、前端 **106/106**、FakeGrbl E2E **32/32** 均通过。Vite 同时输出主入口 `index.html` 与独立打包入口 `voice-private.html`（引用带 hash 的 JS），保持黑屏修复的原导航。Windows `cargo tauri build` exit 0，新 `target/release/moss-desktop.exe` **57,346,048 字节**、SHA-256 `e4db8f4752e3629b1c5fb966fbe351cd10a4ed914f66483ff43f24b9d24c626f`；普通 NSIS **18,120,515 字节**、SHA-256 `7f4fc5f1503ac3ee27bfdb171d70f9f49e09321c25cb99a77e933373b1fe0a5e`。Android APK `cargo tauri android build --target aarch64 --apk` exit 0，**51,975,686 字节**、SHA-256 `d7879a21b1b11c9be80919085e9c1eb3ea5b0874d0783811c1ae0626a2deab74`，APK 内 arm64 `.so` 与当轮编译库 SHA-256 均为 `74819d3122e0a3476c78eb8b415f7ba8b41c9a3f28bd1ff29491257feae9b30a`。**本轮尚未在真实 Windows WebView2 手动点击新隐私窗口做 ACL 验收**：不得将编译/模拟 PASS 冒充已实际显示私人记忆；普通 NSIS 也非 NV18 专用 sidecar 安装包。未读写用户真实记忆、凭据或连接激光设备。见 `docs/integration/voice-private-acl.md`。
- **并行构建后的交付目录复核**：在本轮发布成功后，检测到同一工作区存在其他 cargo/release 编译进程，同名 EXE 和 NSIS 曾被随后覆盖，故以上首轮哈希不再代表当前磁盘文件；**没有终止其他编译任务，也没有用缓存/旧产物假冒本轮回执**。等待全部 cargo/rustc/link/makensis 进程退出后再重复校验，当前稳定 `target/release/moss-desktop.exe` **57,359,360 字节** SHA-256 `bfb893b9c817db2125a230f56ee6870b05772ba6889c594be5318f3aa36af034`；普通 NSIS **18,119,867 字节** SHA-256 `8265837aa34e2c402790e1f745efc615045b5cbd2e096af9e0eae3f4e3e62116`。两者可能包含并行开发流程的其他更新；仍须对**当前磁盘最终 EXE**做一次 Windows GUI 的“语音中心 → 打开本地安全设置窗口”真实 ACL 验收，确认版本与功能。因同机并行 Rust 构建，本轮**没有清除共享增量编译缓存**，避免破坏其他任务。
- **关联**：`apps/desktop/src-tauri/{build.rs,src/lib.rs,src/voice_builtin.rs,capabilities/{default,voice-memory,voice-plugins}.json}`；`frontend/{vite.config.mjs,voice-private.html,src/voicePrivate.jsx,src/components/VoiceBuiltinCard.jsx,src/voiceApi.js,tests/voicePrivacyAcl.test.mjs}`。

### NV15 Home Assistant 令牌修订与 HTTPS 只读发现（2026-10-11；控制保持禁用）

- 新增 `RuntimeDb::home_token_status/update_home_token_if_revision`：SQLite `app_secrets` 独立保存令牌及修订、expected_revision 原子更改、替换/撤销立即使待审 HomeControl 记录失效，不写入普通设置/回执/日志；**现有凭据表未证明系统级静态加密**，正式安全存储仍待审。
- 桌面 main-only `voice_home_token_save`/`voice_home_discover` 已与 Tauri ACL、前端保存/更换/撤销凭据及手动只读同步按钮接线。`moss_voice::home_readonly` 仅固定 HTTPS `/api/states`、`/api/services` GET，无代理/重定向，3 秒连接及 10 秒总超时、响应上限 1 MiB、只接受 200 JSON、只显示勾选的实体和安全属性；查询后重查白名单及令牌 revision，变化即丢弃结果。不能发控制 POST。
- 模块单测：只读状态/URL 3/3，SQLite 身份/撤销等新集成 4/4；Tauri 受限命令测试、前端 103/103、Vite build、FakeGrbl 32/32 和 Rust workspace 628/628 已通过。Windows release lib build 成功；但同一 Windows 上运行中的 EXE 占用安装包路径，且另一个 GUI 工作流发现 loopback WebView 的私有 ACL 拒绝问题：**尚未实测该令牌配置/只读同步在实际 Windows GUI 的完整闭环**。Android APK 51,898,198 B sha256 `99ad33d0aab423566cd1c7b30fe6311aaf853b05a5fa014b4861805def173150`（构建时点需与后续源码分开核验）。
- 无真实 HA 设备、个人 token、登录或服务 POST；HomeControl 专用正式审批/审计/执行链尚缺，生产控制不能启用，NV15 继续 partial。


### 修复 Windows 实机 GUI 白屏：后端缺少嵌入的前端资源（2026-10-11）

- **现象/影响**：直接打开编译出的 Windows `target/release/moss-desktop.exe`，主窗口几乎全白，显示 `{"result":"未找到路径","success":false}`；同机 `GET http://127.0.0.1:9666/` 实测 HTTP **404**，`/draw` 实测 HTTP 200 但 HTML **0 字节**。左侧导航根本未渲染，“语音中心”无法进入；仅通过浏览器/Vite 模拟的组件测试不足以覆盖这个发布构建问题。
- **根因**：Tauri Desktop setup 会 `navigate({backend_origin}/)`；后端 `laser-api::FrontendFixture` 基于 `rust-embed` 在 Rust 编译时将 `frontend/dist` 嵌入二进制。原版 `tauri.conf.json` 未规定前端先构建，`laser-api` 也没有前端 dist 变化时的 Cargo 重新编译指示，允许嵌入缺失/过期的 `index.html`，造成发布 EXE 存在但首页 404。
- **修复/门禁**：`apps/desktop/src-tauri/tauri.conf.json` 添加 `beforeBuildCommand`（`../../../frontend` 执行 `npm run build`）；新增 `crates/laser-api/build.rs` 监视 `../../frontend/dist` 并在 `release` 模式拒绝缺少 React 根 HTML、Vite 引用或非空 JS 的构建；`laser-api` HTTP 静态资源测试新增 `GET /` 必须返回 React HTML 的断言。原 React 经典 JSX 导入修补与 VoiceErrorBoundary 保留不变，无新的激光/语音硬件操作。
- **实际 Windows GUI 复测**：关闭本次验收运行的旧 EXE、重建 Windows Tauri release（exit 0）并启动新程序；`GET /` 实测 HTTP **200、679 字节**、入口 JS **200、1,605,547 字节**。Win32 真实桌面 screenshot 中完整工作台和左侧导航可见，随后实际点击“语音中心”进入紫色设置页，页面持续显示且等待至少 **10 秒**进程存活，无整窗黑屏。最新 `target/release/moss-desktop.exe` **57,145,344 字节**，SHA-256 `8162e618552c9db5f77e858ca84ef280c53b4ec3bef0534f370bbc8650101d2e`；普通 NSIS `MOSS_0.1.0_x64-setup.exe` **18,073,917 字节**，SHA-256 `b0027715a5907736a55cbef6caf3c6860dd1deaaf527822f728722ccf9c4c846`。
- **自动回归与 Android**：静态路由专项 **1/1**、全工作区 Rust **628/628**、前端 `npm test` **103/103**、FakeGrbl E2E **32/32**，均成功，无物理设备连接。Android `cargo tauri android build --target aarch64 --apk` 退出 0；产物 `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` **51,898,190 字节**、SHA-256 `688432db1189496584a0e3718fb64533efe53082fe149e2c967928fd074309eb`，其内嵌的 `lib/arm64-v8a/libmoss_desktop_lib.so` 与当轮编译 `.so` SHA-256 **均为** `1ad5d2706b86d185f0a0774855d2fce9561c1e1f3a3ee370c900bdafee80559f`。Android 构建不等于设备安装测试；Windows GUI 黑屏另已直接实测。
- **未完成的独立 ACL 问题**：真实“语音中心”虽然不再黑屏，但 `voice_plugins_status`、`voice_memory_list` 返回 `not allowed by ACL`。当前 Tauri WebView 从 HTTP loopback origin 加载，敏感记忆/插件 capabilities 原本限定 `main` 打包页面且没有 remote origin，不能简单全放开 `127.0.0.1:*` 消除报错，应独立设计可信页面与本机后端信任边界。不代表语音/激光实机或 NV18 特别安装包验收完成。
- **关联**：`apps/desktop/src-tauri/tauri.conf.json`、`crates/laser-api/build.rs`、`crates/laser-api/src/lib.rs`（并参考 `apps/desktop/src-tauri/src/lib.rs` 的后端同源导航）。

### NV16 个人记忆持久条件更新和删除原子化（2026-10-11）

- **实际生产底层新增**：`RuntimeDb::voice_person_summary` / `update_voice_person_summary_if_revision` / `delete_voice_speaker_if_revision`：必须有登记 speaker 行，按 speaker ID 隔离查询；summary 长度和控制字符拒绝；旧 revision 不能覆盖；删除登记 speaker 与对应个人摘要在同一 SQLite 事务内执行（不依赖外键 pragma）。不允许自动回退为设备共享记忆，也不根据模型自报身份授予访问权限。上层可信识别与 UI 尚未接线。
- **验证**：`laser-runtime/tests/nv16_nv17_schema.rs` **3/3 PASS**（新增双人摘要隔离/旧 revision/未知身份/删除）；`cargo test --workspace` **624/624 PASS**。前端 103/103、Vite build、FakeGrbl 32/32 与 Android APK 结果对应此前本会话状态，**新增个人摘要 DB API 后尚未重新验证新的 Windows 正式发布包**，Windows desktop.exe 仍由用户运行中进程占用；不结束 NV16 Goal、不启用真实声纹。


### NV17 下载状态的 SQLite 条件迁移与旧请求拒绝（2026-10-11）

- **实际软件变更**：`laser-runtime::RuntimeDb::update_firmware_download_if_revision` 新增生产可调用的持久状态 CAS，只允许 Pending→Downloading、Downloading→Downloaded/Unknown 的合法迁移；以 delivery ID、expected revision、前置状态与未上报设备状态联合检查，一次更新只修改一行且版本递增。Unknown 不能被普通重试复活。新增 SQLite 集成测试验证旧修订、非法迁移、跨记录重放全部拒绝。
- **验证**：SQLite 集成 **2/2 PASS**、Rust `cargo test --workspace` **623/623 PASS**、前端 **103/103 PASS**、前端 build exit0、FakeGrbl **32/32 PASS**、Android APK build exit0（51,847,538 B；SHA256 `d59db1e3bd9e0870ff42a55906a2ade3eb7059fbf3442ddd5489cce28de35ef0`）。Windows Rust release 编译到 EXE 替换阶段失败：`target/release/moss-desktop.exe` 被**现有 PID 49120 的运行中桌面程序占用**，Win32 拒绝访问；未终止该用户进程、未替换 EXE、未声明 Windows 新安装包构建成功。
- **安全边界**：NV17 正式 OTA 路由/设备身份绑定还未接入；NV15 真实 HA 凭据审批/操作、NV16 真人模型、NV18 原生第三方许可、NV13 真 Codex 沙箱仍不足以结项；未触碰物理设备或开启不安全执行。NV18 只读许可证审计仍 `release_ready=false`、14 阻塞，不能正式发布。


### 修复 Windows 桌面语音中心打开后整窗黑屏（2026-10-11）

- **现象/影响**：在桌面侧栏打开“语音中心”，整个窗口内容消失，只剩黑色背景；不是激光或语音设备故障。Windows Edge 无头模式 + 本地 Vite + 合成 Tauri/HTTP 状态在修复前复现：React 根节点内容为空，页面抛出多次 `ReferenceError: React is not defined`。
- **根因**：当前经典 JSX 转译要求组件作用域中有 `React`，新增六个组件（VoiceHomeCard、VoiceMusicCard、VoiceAgentCard、VoiceVisionCard、VoicePluginsCard、VoiceMemoryCard）仅引入 hooks 没有导入 React；渲染抛异常时缺少错误边界，React 卸载整棵树。旧/异常 MCP 状态缺少数组时 VoiceMcpCard 也可能在 `.length` / `.map` 抛异常。
- **修复**：六个组件补充 `React` 导入；MCP 卡片校验 servers/connections/audit 数组并设置默认值；在 VoicePage 外新增 VoiceErrorBoundary，发生渲染错误时保留主菜单、顶部栏和返回/重试按钮，UI 不回显异常消息或敏感配置。新增 JSX 导入静态回归测试防止遗漏。
- **验证**：`frontend/tests/voice-center-render.smoke.mjs` 在独立浏览器用完全合成的 Tauri 和 HTTP 数据复现前后差异；修复后开发模式与构建后的 Vite production preview 均覆盖正常响应、缺失 MCP/音乐/内置状态及强制卡片崩溃，正常页面渲染、无未捕获 JS 异常，故意崩溃显示可见隔离页且能返回工作台。`npm test`、`npm run build`、FakeGrbl E2E **32/32**、`cargo test --workspace`（退出码 0）均通过；Windows 正式 `cargo tauri build` 退出码 0，生成新的 `target/release/moss-desktop.exe`（48,658,944 字节，SHA-256 `363dc01432e3768dd9ef541150dffe7eb0fb1bd71c6b60b3331d22be5da4cfe7`）与普通 NSIS `MOSS_0.1.0_x64-setup.exe`（15,611,453 字节，SHA-256 `d0fa593121b684b137419e744e626d40be4a5dce54d9fec76f4de831934fc620`）；Android `cargo tauri android build --target aarch64 --apk` 退出码 0，APK 51,847,538 字节，SHA-256 `868de48510c020ee14d5d566824cbb96c98472a04404d0ef2ae9655347994bb4`，APK 内 arm64 Rust `.so` 与当轮编译库 SHA-256 `71c833bd983e79a367ea47b7ce9b3254d53120922156cd45600d5bb0c3fcaa35` 完全一致。未访问用户真实设备/个人语音设置，不等于已实际打开用户已安装旧版 EXE 验证 WebView2；普通 NSIS 不包含 NV18 语音辅助程序，许可证审计仍阻止发布。
- **关联**：`frontend/src/components/Voice{Home,Music,Agent,Vision,Plugins,Memory,Mcp,ErrorBoundary}*.jsx`、`frontend/src/main.jsx`、`frontend/tests/voice-center-render.smoke.mjs`、`frontend/tests/jsx-classic-react-import.test.mjs`。

### NV16/NV17 正式 SQLite 元数据迁移与固件失败恢复（2026-10-11）

- **问题**：NV16 声纹身份和独立按人记忆、NV17 固件下载状态此前只存在合成内存，无法在重启后独立保留；下载过程中进程退出容易被错误认为可以自动重复操作。
- **实现**：`laser-runtime::RuntimeDb` schema **v8 → v9**，事务性建立 `voice_speakers`（模型、维度、embedding 元数据）、`voice_person_memory`（按人主键隔离）与 `voice_firmware_delivery`（下载/设备报告两个状态）表；`recover_firmware_downloads` 启动恢复将 `downloading` 改为 `unknown` 且增加 revision，绝不标记安装成功或自动重试。同步兼容旧 v4/v5/v7/v8 模拟迁移的回归测试，新增磁盘 SQLite 关闭再打开集成测试。
- **验证**：`cargo test -p laser-runtime` 与 `--test nv16_nv17_schema` 已通过；新一轮 `cargo test --workspace` **622/622 PASS**、`npm test` **103/103 PASS**、前端 Vite build exit0、FakeGrbl E2E **32/32 PASS**，Windows `cargo tauri build` / Android `cargo tauri android build --target aarch64 --apk` 均 exit0。产物 SHA256：`target/release/moss-desktop.exe` 48,658,944 B `32b96a1d2f654f4a5debd9a05ef6b85e1fced93f6a74ec5ccd9be4c2091668bb`；常规 NSIS `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` 15,606,799 B `1deb01931098b390a3459d7038201c684986519d451328c91dd0963f1f16ca52`；Android APK `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` 51,847,538 B `868de48510c020ee14d5d566824cbb96c98472a04404d0ef2ae9655347994bb4`。同一工作区另有并行 UI 变更，哈希对应构建结束时的实际产物，不能视为仅本项变更的独立构建。仍未接入真实 speaker extractor/HA 凭据/OTA HTTP 或独立合规许可；NV15/16/17/18 未达全部完成标准，绝不可由构建结果直接发布或控制设备。


### NV17 固件下载与设备安装上报独立状态（2026-10-11，离线）

- **现象/风险**：仅凭固件下载响应成功推断设备已经安装，会导致界面误报升级成功；中途断线后若自动重试又可能触发重复刷写。
- **修复**：`moss-voice-server/src/firmware.rs` 添加私有 FakeDelivery 状态机；下载 Pending/Downloading/Downloaded/Unknown 与设备 Reported 独立；中断转 Unknown、不可自动重试；仅匹配设备身份和已下载固件哈希可模拟确认安装。该实现没有生产路由、持久数据库、实际传输或 OTA，不能当作 NV17-4 完成。
- **验证**：新增中断重放、跨设备上报、哈希替换、下载与安装区分离线 Rust 测试；`cargo test -p moss-voice-server firmware --lib` **9/9 PASS**、`cargo test --workspace` **621/621 PASS**、前端 `npm test` **102/102 PASS**、Vite build exit0、FakeGrbl E2E **32/32 PASS**。Windows release `moss-desktop.exe` 57,081,344 B SHA256 `7f028e7b6378ca06dd31a6a8a8a655c40b19131b10a768b2715870eab7d29e3d`；常规 NSIS 18,079,196 B SHA256 `3891aed3c20b1d6514196b9c4f77910c75f303fdef65b746c45e176fd716691d`；Android aarch64 APK 51,840,938 B SHA256 `045f5b39a0fe18aae33d291bb23a942db73e6dac2b94b5a9b680b4bf7672ab3d`，双端构建 exit0。常规 NSIS 不含 NV18 helper，许可证审查仍阻止发布；实际设备固件继续禁用。


### NV15 普通语音设置保存不能绕过 Home Assistant 独立授权（2026-10-11）

- **现象/根因**：此前 `save_all` 在新安装尚不存在 `builtin_home` 字段时仍沿用调用者传来的 `voice.builtin_home`，普通设置保存接口可能注入未授权 HA 白名单或 `enabled:true`，不符合只能经版本化 Home 写入的安全契约。
- **修复**：普通保存入口在目标文件尚无 HA 配置时强制写入默认关闭、空白名单；只保留磁盘上已有 HA 配置及 revision，不从普通请求创建控制策略。专用 `save_home_offline` 仍是唯一修改入口。没有启用 HA 网络、Token 或生产控制。
- **验证**：新增 `nv15_generic_settings_cannot_introduce_ha_policy_on_fresh_install`，`moss-voice nv15_` 专项 **7/7 PASS**；`cargo test --workspace` **620/620 PASS**、前端 `npm test` **102/102 PASS**、Vite build exit0、FakeGrbl E2E **32/32 PASS**。Windows `cargo tauri build` 和 Android `cargo tauri android build --target aarch64 --apk` 全部 exit0；`target/release/moss-desktop.exe` **57,081,344 B** SHA256 `e4bf03308b4404c9d936bad1b1c77eb88990f56b74f3115906279332e511d8d6`；常规 NSIS `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` **18,088,733 B** SHA256 `e9f849859fdc3c84a63d1326900f1123e7082021270b0fb37eb355a68e054a75`；Android APK `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` **51,840,938 B** SHA256 `ccdf2be3ca2160c09ed1d0bfdbacfe1e461c53aaf0c32a83d4e376f7b2e17e48`。不等于真实 HA/小智/激光验收；NV18 原生许可阻塞，不能公开分发。


### NV16 离线按人记忆隔离合成回归（2026-10-11）

- **问题与边界**：原声纹 fixture 只有合成登记/匹配/删除，没有覆盖不同人摘要的隔离、未知身份拒绝、旧版本覆盖和删除撤销后不可读取；真实声纹识别仍无模型和标定，不能用匹配提示做加工授权。
- **补充实现**：`moss-voice-server::speaker::SyntheticPersonMemory` 是私有内存模拟器，按已登记的合成 speaker ID 分区，拒绝未标定、unknown、注销身份和旧 revision 写入；删除会移除摘要。未添加生产音频/文件/数据库接口，不能接任意模型自报身份，真实身份信任仍须独立设计。
- **验证**：`cargo test -p moss-voice-server speaker --lib` **5/5 PASS**（新增隔离与版本拒绝专项）；`cargo test --workspace` **619/619 PASS**、`npm test` **102/102 PASS**、前端 build exit0、FakeGrbl E2E **32/32 PASS**。Windows 和 Android release 均 exit0：`target/release/moss-desktop.exe` 57,080,320 B、SHA-256 `f61da3f6df2faff0b25b36404b0ed795c392d18cc89f74ff05e5adcf5fe4f41a`；常规 NSIS `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` 18,083,246 B、SHA-256 `87fe346663ccff4c8e77fd0518bb983e989db38af8ffb05b5f23df8e9e9b184b`；Android APK `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` 51,840,938 B、SHA-256 `d84f79b3b08b24b957c82c4c4c7c2c7d47f03551d56027905cad194896c7f9db`。常规 NSIS 不含 NV18 GPL helper，依赖许可审计仍未通过，不可公开分发；真人同人/异人、噪声与重放 FAR/FRR、模型版权及持久化仍未验收，NV16 继续 partial。


### NV15 Home Assistant 离线白名单版本冲突防护（2026-10-11，真实家居仍禁用）

- **现象与影响**：原桌面 Home Assistant 保存调用整个 VoiceSettings 写入，两个桌面窗口的旧白名单可覆盖新设置；其他语音设置保存也可能把已经移除的实体重新写回，造成未来权限复活风险。
- **根因**：离线设置无独立 revision、缺少 expected_revision 条件写入；通用 save_all 只保留 MCP 独立设置，不保留 HomeAssistant 白名单/版本。
- **修复**：在 settings 文件的 `voice.builtin_home_revision`（旧配置为 0）引入有界整数版本，`read_home_offline` 损坏配置 fail-closed；`save_home_offline` 共用全局写锁，检查预期版本并只替换 HomeAssistant 配置和版本，其余 voice/root 配置保持；相同配置写入不增版本；通用 save_all 保留已存在的 HomeAssistant 版本与白名单。Tauri 主窗口 `voice_home_status/save` 提供版本、409 冲突、无法读取时拒绝保存；前端显示版本并提供显式重新加载按钮，拒绝自动覆盖或重试旧表单。
- **验证**：新增 NV15 多窗口并发、撤销重放、文件损坏与老配置兼容专项 **6/6 PASS**；最终 `cargo test --workspace` **618/618 PASS**、前端 `npm test` **102/102 PASS**、Vite build exit 0、FakeGrbl E2E **32/32 PASS**。Windows `cargo tauri build` 与 Android `cargo tauri android build --target aarch64 --apk` 全部 exit 0：`target/release/moss-desktop.exe` **57,080,320 B** SHA-256 `f24222fb162f2b85685d557d59968ad766bf2ebd41e9a32b0d9bf83f11d70f44`；常规 NSIS `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` **18,079,691 B** SHA-256 `88fc8b0444095144f7032bb731ceaa337bca794b4fc5add2af361c83f805a85a`（不含 NV18 helper）；Android APK `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` **51,840,938 B** SHA-256 `bdaeddbcc91ba061b1e972730c01bcbadbe6f51710a21cf332a60533f0a488d8`。无真实 Home Assistant 网络控制/token/家居操作，仍禁用生产控制；未做安装与真实设备验收，NV18 第三方许可证仍阻塞分发。
- **关联**：`moss-voice::settings::{read_home_offline,save_home_offline,save_all}`、`voice_builtin.rs`、`VoiceHomeCard.jsx`。实际 HA 授权、专用审批 UI 和真实设备统一仍为 NV15 partial/FINAL。


### NV17-3 测试专用固件下载票据和撤销演练（2026-10-11，生产 OTA 未修改）

- **目标/风险**：NV17 原有合成固件文件登记已经覆盖 SHA 和默认禁用，但尚未验证下载票据绑定设备、认证版本、过期/撤销和并发限制，不能据此直接添加真实固件下载路由。
- **实现**：仅 `#[cfg(test)]` 新建 `firmware/download_mock.rs`；使用 32 字节 OS 随机一次性 token（内存只存 SHA-256），绑定合成设备/客户端、固件 board/version/hash/record revision、模拟凭据 revision、许可原子开关及最长 5 分钟 TTL。最多 16 个待签票据、全局 2/设备 1 个并发下载，以 64 KiB 分块向**测试闭包**发送合成字节，每块重验许可，取消后停止；文件仍由原先哈希/长度验证的模拟 store 读取。**未连接任何 OTA 路由、真实设备凭据、HTTP、文件推送或刷机。**
- **验证**：`cargo test -p moss-voice-server firmware --lib` **8/8 PASS**（含原 4 个加新增 4 个），覆盖身份错配、重复消费、不同修订、票据过期、禁用/篡改、并发上限和传输中撤销。生产代码保持 OTA `firmware:None`、推送关闭；真正设备 token、签名 URL、可信恢复与实体刷机仍待验。仅测试/文档模块，依 AGENTS.md 例外不要求该项单独重建 Windows/Android。
### NV18-3 实测原生依赖许可不完整，增加 fail-closed 发布审计（2026-10-11，审计脚本-only）

- **现象**：上轮带 GPL helper 的专用 NSIS 虽已经通过完整解包/源码重建测试，但原生静态库许可证及对应原生源码未验证，不能误判为可分发。
- **根因证据**：本机 `sherpa-onnx-sys 1.13.8` 的 `build.rs` 明确静态链接 13 个库（含 `espeak-ng`、`onnxruntime`、`piper_phonemize`）。本轮 native archive **123,206,268 bytes**，SHA-256 `56ffcf3c454c1f14f7bc9887286cc8143e7e542dc632804e1c447d5f8d534eaf`；解压有 14 个 `.lib`，但许可证文本 **0**，现有 notices 未涵盖 12 个被链接组件名称。Cargo crate 自身的 SPDX 字段不足以证明原生静态包许可完整。
- **加固及验证**：增加只读 `scripts/audit-nv18-native-licenses.ps1`，默认打印完整缺口且不改文件；`-RequireComplete` 要求 archive/hash、库名、NOTICE、审核清单及对应源码/许可文件的真实 SHA 均合格，缺一即失败。实测 `release_ready=false`、**14 项阻塞**；Strict Gate 按预期拒绝（fail-closed），解析/执行测试通过。真正源码/许可仍未补齐，不安装/签名/外发 NSIS，也不触碰设备。
- **范围**：仅新增审计脚本/文档，不修改生产功能或依赖；按 AGENTS.md 测试文档例外，未重建 Windows/Android。本轮不能宣称整体任务完成。审计详情见 `docs/developer/nv18-source-license-audit-2026-10-11.md`。
### NV15 白名单别名与实体 ID 冲突防护（2026-10-11，软件安全修补）

- **现象与影响**：离线 Home Assistant 白名单允许一台设备的语音别名与另一台设备的完整实体 ID 同名，例如将 `light.desk` 的别名设为 `switch.fan`；目录查询先命中别名，导致按另一台设备 ID 查询时可能选错目标。全空白别名在内部目录校验中也未拒绝。当前生产控制仍被禁用，风险发生在合成目录/未来执行器接线前。
- **根因**：`HomeCatalog::lookup` 按别名优先查询，目录构建和桌面离线配置校验只检查别名彼此重复、实体 ID 彼此重复，没有统一检查别名与**其他实体 ID** 之间的冲突；内部目录未对别名做 `trim` 后的非空校验。
- **修复**：离线配置保存校验拒绝大小写变体的重复 ID，保留暂时停用的实体 ID，并禁止别名伪装成**任何其他合法 HA 实体 ID**（即使该 ID 从未同步或加入白名单）；内部目录再次 fail-closed 校验，拒绝空白别名，并让精确实体 ID 查找优先于别名。实体自己的完整 ID 仍可作为自身别名。没有新增真实 HA 网络访问、控制执行或审批放行入口，默认 `enabled=false` 不变。
- **验证**：新增 `aliases_cannot_shadow_another_home_entity_id` 与 `nv15_saved_aliases_cannot_shadow_another_entity_id`，覆盖已选/未选/尚未出现的完整 ID、大小写/前后空格、停用后重新启用、自己 ID、误控制目标与空白别名；`cargo test -p laser-host voice_home --lib` **22/22 PASS**，`cargo test -p moss-voice nv15_ --lib` **3/3 PASS**，前端 `npm test` **102/102 PASS**、`npm run build` exit 0。最终补强后 Rust workspace **615/615 PASS**、FakeGrbl E2E **32/32 PASS**；本轮 Windows EXE/常规 NSIS、Android aarch64 APK 后续正式构建均 exit 0（精确 SHA 见下条）；真实家庭设备和真人操作 **unverified**。
- **关联**：`crates/laser-host/src/voice_home.rs::{allowed_alias,HomeCatalog::from_snapshots,HomeCatalog::lookup}`、`crates/moss-voice/src/settings.rs::BuiltinHomeAssistant::validate_offline`；NV15-1/2 的离线白名单边界、`docs/integration/moss-fusion-implementation-todo.md`。不更改 SQLite schema、Tauri 命令契约或凭据读取。
- **本次最新交付构建核验（仅生成，不安装/外发）**：`cargo tauri build`（x64）exit 0，`target/release/moss-desktop.exe` **57,097,728 bytes**、SHA-256 `3b9b4cc67796be8001c4e382817872040d7032ba92a6e230286c635bbdb6d039`；**常规 NSIS** `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` **18,092,999 bytes**、SHA-256 `997fc067a37bb1781408da5070aefad6b69bb171cabf8b5d15e1aa74b60c0dec`（不包含 NV18 GPL helper）。`cargo tauri android build --target aarch64 --apk` exit 0，APK `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` **51,840,922 bytes**、SHA-256 `f81b32d852167799011ff40338d5f5fca634a8dc0e4d739efd330480620ab14c`；编译的 arm64 `.so` **46,119,856 bytes**、SHA-256 `766d621dc5c58afd5f6f28a4f5623a6a4a45c162970ad788033c17511c16eee1`。存在非阻塞 Rust linker/unused mut 及 Gradle 8.14.3 deprecated API 警告；尚未在干净 Windows VM 安装、运行 Android 实机或核验含 helper 专用 NSIS 与这次变更对应的复现，且 NV18-3 原生许可 `release_ready=false`，任何包都**不对外分发**。

### NV15–NV18 本轮完整跨端软件回归与发布复核（2026-10-11）

- **软件测试**：`cargo test --workspace` **609/609 PASS**；NV15 合成 HA HTTP **21/21**、NV16 synthetic speaker **4/4**、NV17 synthetic firmware **4/4**、其余 Rust 例程均通过；前端 `npm test`、`npm run build` 通过，FakeGrbl 软件 E2E **32/32 PASS**。无真实激光、HA、声纹采集、OTA 刷写、模型服务或小智硬件。
- **常规 Windows/Android**：`cargo tauri build` exit 0，`target/release/moss-desktop.exe` 57,093,632 bytes / SHA-256 `876515ab899bd4082261ad041404d478a634e16e78de48228960bfc2b1e7da1c`；**常规 NSIS** 18,086,745 bytes / SHA-256 `1a2dcc6faad1e5b7add1d88c3018dd381303aade82fd227a0b8094d2e3f24667`（不带 NV18 helper）。Android `cargo tauri android build --target aarch64 --apk` exit 0，APK 51,840,922 bytes / SHA-256 `e8ab2693e08e3a86095843b11e4a5f69a6b29618246355718c4faf19cc48de3f`，内置 arm64 Rust `.so` 与本轮编译库均为 SHA-256 `eee05d31a9aa94f3e20e32b0f4c444b2f451fc7598ce75337997ec6c84dc4571`。
- **专用 NSIS 比对进一步关闭**：`verify-nv18-nsis-in-docker.ps1` 严格断言 7 项必需文件存在、无 .env/数据库/密钥/target 缓存。NSIS 内 helper **完整 SHA-256** 等于发布清单 `d8f8db26f10f3df26d205f427470451333cb714e13e68aefd4ae1c75577fa4b2`。桌面 EXE 打包前 57,088,512 bytes SHA-256 `391023deda99f3a668c52502725c5ad538fc6cfcf0000fb562ffe893fc36a23b`，NSIS 内同样长度、SHA-256 `5ee249705fa5fdb99526a40fd8f01597b1dcc5064e63815f99d59900cfad6095`；逐字节 `cmp` 仅见 Tauri 固定标记 `__TAURI_BUNDLE_TYPE_VAR_UNK` → `NSS` 的 3 字节修改。脚本要求差异数**恰好3**、连贯、旧新 octal 完全匹配、位于指定标记，再将打包前 EXE **仅在内存中改这3字节重新 SHA-256**，与 NSIS 内 hash 一致；任何其他变化均 fail-closed。检查容器、Docker 镜像与文件完全清理，没有安装 EXE。
- **GPL helper 源码及依赖复核**：发布包暂存源码在公开 crates 缓存补齐后，完整 `cargo metadata --locked --offline` PASS；从**暂存源码** `cargo build --release --locked --offline --target x86_64-pc-windows-msvc` PASS，重新编译 helper / 协议代码（20,732,928 bytes，SHA-256 `71ca860a1aba08777eca56b5a28c0e7e7e35c8711fb53b32d177dc61fe1b1de5`；与打包 helper digest 不同，**未通过字节级可复现构建**）。Cargo 元数据列出 **147** Rust 包、**0** 缺 `license` 声明。`sherpa-onnx` 原生静态包、`espeak-ng` 对应源码/许可文本及分发义务**尚未审核完成**，不得称外部分发合规。详情 [`NV18 独立审查`](docs/developer/nv18-source-license-audit-2026-10-11.md)。未安装/升级/卸载或签名发布。
### NV18 带语音 helper 的 NSIS 无损构建与独立包内校验（2026-10-11）

- **原风险及修复**：旧 `scripts/build-windows-installer.ps1` 曾对 `bundle-resources/voice-local/` 整目录 `Remove-Item -Recurse -Force`，会误删原有未提交源码/许可资源。现入口立即转交 `scripts/build-nv18-sidecar-safe.ps1` 并返回，旧代码仅保留审查、不再进入删除路径。新脚本使用随机唯一命名的 helper 二进制、源码/许可证资源目录和 `target/nv18-<id>/` 发布清单，不删除/覆盖旧资源、不安装程序。失败则中止，不以旧 NSIS 结果冒充新构建。
- **实际构建/清单**：专用 `cargo build --release --target x86_64-pc-windows-msvc` 的 GPL helper 成功且 `--version` 返回 0；`cargo tauri build --target x86_64-pc-windows-msvc --config <本次独立 overlay>` exit 0。新的桌面 EXE **57,088,512 bytes** SHA-256 `391023deda99f3a668c52502725c5ad538fc6cfcf0000fb562ffe893fc36a23b`；helper **20,732,928 bytes** SHA-256 `d8f8db26f10f3df26d205f427470451333cb714e13e68aefd4ae1c75577fa4b2`；**带 helper 的专用 NSIS** `target/x86_64-pc-windows-msvc/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` **23,624,916 bytes** SHA-256 `5a662f052d95992f1089eb88f943fa193304bf462faf85c35d79c4d14892e036`。新版 `target/nv18-24d485ff5a9f4bfb8e4d7c958dbb6fa6/release-manifest.json` 与三个文件的大小/哈希交叉复核通过，记录产品版本 0.1.0、helper 协议3、DB schema8、源码树 SHA-256 `fcfd47fc431ef5c4bb9e4ce7bae6b2e80c61d9867de8c784bc6c64a30839cbf3`，状态 `built_not_installed_or_license_audited`。
- **独立包内核查**：新增 `scripts/verify-nv18-nsis-in-docker.ps1`。从已有 Ubuntu 22.04 镜像的**官方 GPG 验证 APT**临时生成仅供验包的 p7zip-full 容器镜像，以 Docker **禁网/只读/非 root/no-new-privileges/cap-drop-all** 模式列出生成的 NSIS **22 个 archive 条目**。实际确认 `moss-desktop.exe`、`moss-voice-local.exe`、`licenses/voice-local/COPYING`、`THIRD_PARTY_NOTICES.md` 与 `licenses/voice-local/source/apps/moss-voice-local/src/main.rs` 位于包内，未运行/安装 NSIS。测试镜像、临时容器和目录已清理。
- **交付边界**：本轮独立解包证明资源**确实在 NSIS 内**，但还不是干净 Windows 上的安装、启动、升级、卸载或 sidecar hello 实测；也**未完成 sherpa-onnx/espeak-ng 及其静态依赖的全部对应源码/许可审计**。现阶段不得对外分发或声称 GPL 合规完成。普通 NSIS 与专用含 helper NSIS 需分别记录，不得互相替代；签名/正式发布和实际设备听音仍待单独授权。
### NV17 固件登记与升级资格的合成安全核心（2026-10-11，partial / 禁止 OTA）

- **边界与问题**：现有 OTA 固定 `firmware:None`，设备提供的 board/type/version 或 URL 均不可信，不能从自报字段直接提供可刷写固件。新建 `crates/moss-voice-server/src/firmware.rs` 私有**无路由**夹具，不更改 `/xiaozhi/ota` 的响应，不向真实设备传输字节。
- **实现**：仅从受信方已打开的只读输入流导入临时合成 `.bin`，每文件最多16 MiB，总量最多256 MiB；同目录 create_new staging、流式 SHA-256 和文件 sync、硬链接无覆盖原子提交，按**原始型号完整 SHA-256**生成文件名，版本数字逐段规范化，禁止路径分隔符和非数字版本，同型号同版本不同 hash 拒绝。记录默认 disabled，必须完整匹配 sha/revision 才能改变合成状态；每个假下载还要复核允许标志、device/client 非空、型号、版本确实更高、记录 revision 和当前文件 hash；篡改/变更返回拒绝，复制中失败清理本次 `.part`。
- **测试及缺口**：首次模拟发现设备版本 `garbled` 被旧版本比较函数误认为可升级，已新增客户端版本规范化**不合法一律无 offer**，且合成读取仍需严格更高版本。`cargo test -p moss-voice-server firmware --lib` **4/4 PASS**。本模块未持久登记、无设备 token、下载签名/并发限额、HTTP 路由、真正固件格式/板型认证或刷写；OS 句柄防 TOCTOU 和恢复仍须正式设计验收。NV17-1..4 软件仅起步，NV17-5 实机固件刷写留 FINAL，绝不自动启用。
### NV16 纯合成声纹匹配与默认禁止授权门禁（2026-10-11，partial / 无采集）

- **阶段目标与限制**：声纹模型 URL、SHA-256、许可证、embedding 维度和真人误识别率尚未锁定。直接接入设备 ASR、登记真人声音或用调试阈值放行加工存在误判风险；因此**没有运行声纹模型、读取麦克风、保存录音/人脸/向量或把身份接入生产审批**。
- **软件实现**：`crates/moss-voice-server/src/speaker.rs` 新增纯合成 64..1024 维向量检查，三段每段 2..10 秒的元信息约束，逐段归一化、三段两两余弦一致性、均值二次归一化；无效长度/零向量/NaN/Inf/削波/多说话人拒绝。姓名/ID/模型 ID 格式严格限制；重复登记拒绝，删除模板使任何调试校准失效。模型不匹配、上一轮 generation、最高分不足和 top1-top2 差距不足一律 `Unknown`；全局最多 2 个模拟占位任务，同设备仅 1 个、等待队列最多 16、观测超时 1.5s。**未校准时即使向量完全一致也 `Unknown`；`may_authorize_action()` 永远返回 false。**
- **验证与剩余问题**：`cargo test -p moss-voice-server speaker --lib` **4/4 PASS**，全部使用人工一位热编码向量，无个人生物识别数据；真正 GPL helper 提取器、模型签名/资源预算、协议、数据库、按人记忆/UI、真人 FAR/FRR 标定、重放攻击与激光身份门禁仍未实现。不得将模拟 matching 认定为已具备身份认证能力。
### NV15-4 合成 HA 网络控制结果不确定性加固（2026-10-11，mock-only）

- **现象与原因**：测试专用 `voice_home/control_mock.rs` 原本在 HA POST 发送后，遇到响应中断/状态读取失败会返回普通 `ServiceUnavailable`；调用方无法区分“请求没有发出”与“设备可能已经执行”，存在误用该错误触发重试的风险。状态不一致时只有 1 次 GET，未覆盖规范要求的最多 3 次只读状态观察。
- **修复**：保持本机 `127.0.0.1:0` 的假 HA 服务器及已存在的 SQLite `ApprovalKind::HomeControl` 原子消费，不接入生产网络。POST **只发一次**，发送开始后任何断线、非 200、Content-Type/Location 错误、坏 JSON、过大回复或许可撤销均返回 `ObservationResult::ResultUnknown`；随后最多 3 次 GET 且总时限 3 秒，目标实体不匹配返回 unknown，3 次都未到目标状态为 `AcceptedStateUnconfirmed`。使用真实模拟数据库 `ApprovalService::record_outcome` 写入不可覆盖的 `Unknown` 结果，原审批无法再次消费，避免不确定结果被迟到成功覆盖。
- **测试和安全边界**：新增 POST 已收到但连接无回复、计划/参数篡改或许可被撤销时 0 POST 负向测试；现有正例一次 POST+一次 GET、目标三次不匹配不重 POST、已撤销不发送仍通过。`cargo test -p laser-host voice_home --lib` **21/21 通过**，未使用真实 HA 服务器、密钥、实体或激光设备。真正 HA endpoint、凭据保管、桌面动态同步、在线 HTTP POST 与逐实体授权仍未接入，NV15-4 和 NV15-5 不标为真实完成。
### NV14-2 合成真实音频格式回归（2026-10-11，软件完成；听音 FINAL）

- **原问题**：NV14 已通过合成 WAV、Opus 泵和假小智播放，但尚无有效 MP3/FLAC/Ogg Vorbis 文件夹具，不能仅凭 Symphonia feature 宣称这些格式可解码。
- **修复**：新增 `scripts/generate-nv14-synthetic-audio-fixtures.ps1`，仅从既有缓存的固定 Ubuntu 22.04 Docker 镜像临时安装 Ubuntu GPG 验证的 FFmpeg 4.4.2，生成四个 48 kHz 双声道、1秒 440Hz 纯正弦波格式（MP3、FLAC、Ogg Vorbis、Ogg Opus）。输出存于 `crates/moss-voice-server/tests/fixtures/`，源/文件 SHA-256 记载于该目录 README。无个人音乐、实时网络模型、系统播放器或真实设备输入；用于生成的 Docker 容器、临时镜像及工作目录均已清理，不向 Windows 安装软件。
- **负向及兼容性**：`music/decode.rs` 新增实际编码文件解码至 24kHz PCM 的 MP3/FLAC/Ogg Vorbis 检验，并明确拒绝尚未支持的 Ogg Opus；截断 FLAC 不能错误报告成功。执行 `cargo test -p moss-voice-server music::decode --lib` **6/6 PASS**（其中 3 项为新增格式测试）。任何真人听音、超长资源使用、真实小智设备延迟仍保持 **FINAL/unverified**。
- **完整软件验收**：本轮 `cargo test --workspace` **587/587 PASS**；前端 `npm test` / `npm run build` exit 0；FakeGrbl `npm run test:e2e` **32/32 PASS**（无激光机）。Windows `cargo tauri build` exit 0 并生成新的 `target/release/moss-desktop.exe`（**56,967,168 bytes**，SHA-256 `aa9b3c983a77ee5c306ff5b4259cd159561c1150fe8c386dbdfb23be19901ed7`）及普通 NSIS `MOSS_0.1.0_x64-setup.exe`（**18,055,380 bytes**，SHA-256 `f5b246e3b1bf38310b223bf6782b752bc16d49cbd31f86bdcb7fd2c0d703b8d4`），两个产物确实在本次构建中刷新。Android aarch64 release APK exit 0，`apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk`（**51,785,090 bytes**，SHA-256 `f4d0aa18d51399cd7947353295d2181e545eb86868098adb500589d1a6577605`）；APK 内 arm64 原生库与本轮 Rust release `.so` SHA-256 相同：`5b92265e602e503dfde1d2d411a9499859a3e010ad0542db70f7e6b2b3cf9445`。NV14 可自动完成的软件验收已具备证据，但实际设备听音/长曲/插话延迟及安装保持 FINAL/unverified，普通 NSIS 非 NV18 sidecar 安装包。

### NV15-4 合成控制结果观察、只发送一次（2026-10-11，mock-only）

- 在 `crates/laser-host/src/voice_home/observe_mock.rs` 新增测试专用的控制生命周期：先重校验计划/当前许可，再交由合成传输提交**一次**，最多查询目标状态三次；区分 `Confirmed`、`AcceptedStateUnconfirmed` 与 `ResultUnknown`。已发送后的断线或撤销只能标为未知，绝不自动重发；未授权或撤销前不调用传输。只有假传输 trait，无实际 HA POST、真实账号/设备或生产审批授权。新增 4 个结果观察测试，`cargo test -p laser-host voice_home --lib` **19/19 PASS**。真正 HomeControl SQLite/桌面 UI、实际动作仍保留待办，不得以本模拟器结果冒充真机控制成功。

- **NV15-4 本轮整体回归及发布产物**：`cargo test --workspace` 通过（599 项由运行器识别，退出码0）；前端 `npm test`/`npm run build` 成功、FakeGrbl E2E **32/32 PASS**（模拟器，无真实激光机）。Windows EXE **57,097,728 bytes** SHA-256 `93103f7b552190971b6f1e5b10d7252cd2bb389b71aee45cbeae09cbde7268f8`，普通 NSIS **18,090,301 bytes** SHA-256 `a951cc99026ab6e12e0d09c1c48ddd97442ea50fab8eb8c7a54e4e421adfc0af`，本轮 `cargo tauri build` exit 0。Android aarch64 APK **51,840,922 bytes** SHA-256 `8366bd8ff738eb03d567ce18c4c5b85b6c211c5fc70b31918560c6b10c6f73ce`，build exit 0，APK 内 Rust lib SHA-256 `16992c9656e34af109849d1dea60da2cf4056318d345e6d8220a5bd7530b3b85` 与本轮 `.so` 一致。普通 NSIS 不代表 NV18 专用 sidecar 包；真正 HA 控制仍禁止。

### NV15 Home Assistant 安全策略与离线审批模拟（2026-10-11，partial / 默认不可执行）

- **问题与隔离边界**：如果模型可以自行指定 HA 的域名、实体、服务、温度、媒体 URL 或凭据，可能绕过用户的实体白名单及桌面审批。当前只新增 `crates/laser-host/src/voice_home.rs` 和仅编译于 Rust 测试的 `voice_home/{mock_http,approval_mock}.rs`。**没有生产 HA 控制入口、Tauri 管理页面、真实 HA 登录或 POST 请求**，不与现有激光/ExternalTool 审批共用“确认”许可。
- **NV15-1 只读模拟同步**：synthetic HA 服务经真实 `127.0.0.1:0` HTTP socket 接受固定 `GET /api/states` 与 `GET /api/services`；拒绝自定义目标/方法、302、401、HTML、>1MiB 响应、断线。模型不可决定扫描源，只有 desktop authored 的实体 + alias + enabled 列表进入可见目录，重名或非法实体拒绝；锁/报警/窗帘不能配置免审批。未知/离线实体不能操作。状态属性严格仅保留最小数字/单位字段，丢弃 PIN、Token、私有任意属性及复杂对象。
- **NV15-2 固定控制意图（不发送）**：只为已选择的实体根据真实服务清单构造固定 `/api/services/{domain}/{service}` 服务路径和规范化 body，不提供 POST states、脚本、Shell、事件、模板、任意 URL/Host；灯亮度 0..255，温度必须落在目标实体 min/max 范围，音乐只能选 desktop 本地允许的 `library_...` 媒体 ID，额外参数拒绝。测试覆盖 light、switch、climate、lock、alarm_control_panel、cover、media_player 所有映射；**所有计划强制要求独立 HomeControl 审批**，即便保存了低风险 Allow 也不在当前阶段免审批。
- **NV15-3 仅软件原型**：`approval_mock.rs` 用模拟桌面允许、设备许可标志、一次性随机 ticket、最多30秒 TTL 以及 policy_revision/credential_revision 比对验证审批失效和单次消耗。更换密钥或配置、撤销、服务被移除、重复消费和注入路径都不能放行。**不是**实际 `ApprovalKind::HomeControl`、持久事务、崩溃恢复、真实用户身份、正式凭据存储或 HA POST；`execute_without_home_control_approval()` 一律拒绝。所有模块故意不注册到生产 voice tool/agent/HTTP 路由。
- **离线验收**：12/12 Rust 定向测试通过；均为假实体、假密钥及本机模拟服务，不连接真实智能家居。完整 workspace/前端/FakeGrbl 与 Windows/Android 构建结果另行更新。NV15-4/5 保留后续阶段与明确的逐实体真机授权。

### NV14 本机音乐：安全索引、流式解码与模拟会话状态（2026-10-11，partial）

- **现象与影响**：原有内置语音只有 TTS/回声播放，缺少受控本地歌曲索引和播放源。未经校验的路径会越界读文件，整曲缓存可能占用大量内存，打断后迟到 PCM 还可能播出。
- **修复与默认值**：VoiceSettings 新增 BuiltinMusic(enabled=false,directory="")，旧版升级也默认禁用，不会主动扫描用户目录。music/index.rs 只扫描用户明确指定的本地绝对目录（≤8 层、10,000 首、5,000 目录、50,000 项），跳过符号链接与 Windows 重解析点，拒绝 UNC；单曲≤512 MiB；路径相对 SHA-256 短 ID 不暴露本地全路径；精确/包含/歧义最多5首/安全随机；播放前重查 size/mtime/路径。已增加打开文件真实句柄及 size/mtime 复核，极端并发目录替换场景尚无真实设备压力验收。
- **离线音频处理**：music/decode.rs 使用 Symphonia 0.5 mp3/flac/wav/ogg/vorbis/pcm 解码，按包下混至 mono、复用有状态重采样到 24 kHz i16；每次推 100 ms PCM、最多20个片段队列（≤2秒）、回压时每15ms检查取消，实际数据总量和单包均有上限。music/playback.rs 复用会话现有 Opus PlaybackPump，源替换/撤销清空旧 TTS 和旧歌，generation 不匹配则丢弃；不复制歌词到对话历史。
- **阶段验收**：初始 music 模块10/10，包含中文同名/链接/过期目录/预算、合成 WAV 的24kHz/立体声混音/Opus60ms、队列满取消、双设备隔离、替歌/撤销和索引→PCM→Opus。已继续连通假小智会话与受控工具；真实 MP3/FLAC/Ogg 文件、长曲、实体设备音质与实际安装未验收，NV14 仍 partial。- **相关文件**：crates/moss-voice/src/settings.rs、crates/moss-voice-server/src/music/、audio/pump.rs。Codex 真实执行依旧 DisabledRunner。

- **NV14-3/4 受控点歌和桌面文件夹选择（2026-10-11，partial）**：此前离线音乐索引/流式 WAV 单测不能让真实会话播放，也缺系统目录选择。现有 `MusicTools`、`TurnEvent::PlayMusic` 和 `Session::start_music` 已把只读索引、实际打开句柄、单次播放和 Opus Pump 接通；模型不能传任意路径/URL 或同时调其他工具。每个设备独立 generation、满队列退让和发帧前许可检查保证 abort/自然插话/撤销不重播旧音频，音乐不进入模型历史。补充 `voice_music_pick_directory` 主窗口专用 Tauri 权限及 React 选择按钮，选择只填目录而不扫描，保存显式 enabled 才会索引/重启。已通过内置语音服务 97/97（另外2项真实本地模型忽略）、语音设置50/50（另外1项下载测试忽略）、桌面 main-only ACL、前端 npm test 和 npm build；真实 MP3/FLAC/Ogg fixture、设备音质、长曲与实体安装未验证。本轮完整 Rust/FakeGrbl 和 Windows/Android 正式构建结果待追加。

- **NV14-4 全量软件回归与正式发布构建（2026-10-11）**：`cargo test --workspace` **572/572**、`moss-voice-server` 97/97（2 ignored）与 `moss-voice` 50/50（1 ignored）、main-only capability 单测、前端 `npm test` / `npm run build`、FakeGrbl E2E **32/32** 全部通过。Windows release `cargo tauri build` exit 0：`target/release/moss-desktop.exe` **56,967,168 字节**，SHA-256 `3acb6d04219555277813f52673593b957207de9ff692518c8e9a9cda80e1e2d5`；普通 NSIS `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` **18,057,572 字节**，SHA-256 `4bb71a779cdcfaaa87e3dcb5eb5d1a46706faf6867f512f24f8b1040a427509b`。Android aarch64 release APK exit 0、**51,785,082 字节**，SHA-256 `2071421048e6bf4a6dd0685b2aab74c0219a63f539775001f3b97b8ce65fded7`，APK 内 arm64 原生库与本轮编译 `.so` SHA-256 都为 `a9931a165b6007e49d197f3cf4d78e894685f866bef82f16ad9e5893467d0220`。Xiaozhi console prompt 已按内置音乐条件更新并存档 `docs/integration/xiaozhi-prompts/history/2026-10-11-nv14-music.md`，控制台正文1925/2000、知识库171/200字；外部官方小智 MCP 不会因为该文档自动具备播放工具。**仅合成 WAV/软件模拟测试，无真实 MP3、FLAC、Ogg Vorbis 样本/小智音质、长曲和实体设备验收；普通 NSIS 非 NV18 带 sidecar 的最终安装包**。默认音乐关闭，真实 Codex DisabledRunner 保持禁用。

### NV13 Claude Code / Codex 独立审批框架（2026-10-11，partial：真实执行锁定）

- **现象与影响**：此前语音无独立 Agent 任务入口；直接复用外部 MCP 审批或 shell 执行会绕过目录、凭据及设备撤销门禁。
- **根因**：缺少专用 AgentRun 审批种类、只读意图摘要、可信路径规则、持久任务审计和主窗口配置。
- **修复**：新增 ApprovalKind::AgentRun、ApprovalConnection::Agent、独立 AgentRuns trait 与 AgentCoordinator。ApprovalService.consume_agent_run 在同一 SQLite 事务内检查已批准、实时 voice permit、摘要及有效期后单次消费审批、插入 run、写审计。schema v8 的 voice_agent_runs 保存 50 条有界历史和终态，启动后 running -> result_unknown，不重跑。只读计划采用固定 argv+stdin、受限环境、完整 CLI 路径/工作目录校验；桌面 Agent 卡与 main 窗口专用 Tauri voice_agent_status/save 可保存路径、查看记录。审批展示明确 task/provider/目录名/费用，但不返回内部绝对路径与密钥。激光与外部 MCP 不共享 Agent 执行权限。
- **强制锁定**：REAL_CLI_SANDBOX_VERIFIED=false；默认工具列表不暴露真实 Agent，UI 禁止 enabled=true，生产不包含启动真实 CLI 的进程执行器。help/version 的 mock 核对不等于真实 Windows 沙箱。300 秒超时、Windows Job Object 进程树回收、MCP/账号隔离及取消撤销的真实 subprocess 负向测试尚未实现；NV13-3/5 未完成，统一留安全验收。
- **验证**：CLI 参数/路径/恶意 shell 文本 3 项、schema v8 与恢复 2 项、审批单次消费/撤销、完整 fake runner 语音任务→桌面批准→只执行一次，以及主窗口 ACL 均通过；Rust 工作区 **519/519**、前端单测和正式构建、FakeGrbl E2E **32/32** 通过。Windows release EXE **55,533,568 字节**，SHA-256 `1e58a7189df63043ba61bb1c4afba7cdabb4df15cbdf25ff663fa36d6835fdf7`；普通 NSIS **17,797,509 字节**，SHA-256 `5202bf2f03cf36617c4f78c268c35c524511986b9df6bb0681e3c2982cbcdf1b`，均有明确成功回执。Android aarch64 本轮 APK **51,695,818 字节**，SHA-256 `b673800d760303df69902011cb9bbf9c2ead0d9996695311976489ea1ff399e4`；APK 内 arm64 Rust 库与本轮编译 `.so` SHA-256 同为 `ef6a6fd49fe45ea8f1a4eb39128df7a6908cb32188c709ec7423e0e5bf196da9`。首次 Android 远程任务状态丢失，但本轮新 APK 内 Rust 库哈希一致；随后同命令增量正式构建成功并取得 **exit 0** 回执，Tauri 报告完成 1 个 APK。普通 NSIS 不等于 NV18 语音辅助程序专用安装包。真实 Codex **模型执行和账号授权**、实体设备、安装或提交推送均未进行。
- **NV13-5 真实 CLI 负向验收（2026-10-11，FAIL-CLOSED）**：经用户同意，仅在 Windows 临时工作区、临时 `CODEX_HOME` 和七项白名单环境中执行本机原版 `codex.exe`（`codex-cli 0.157.1`）的版本/help 和不调用模型的 `codex sandbox` 子命令。原版 `exec --help` 具有预期只读/临时/忽略配置参数；真实 CLI 模型任务、用户账号凭据和外网请求均未触达。内置 `-P :read-only`：启动 `cmd.exe` 成功；读取工作目录内、**外**的假 canary 均成功（目录外读限制未通过）；向内、外临时文件写入均被拒绝；受限进程 `curl.exe` 成功连接测试程序自己的 `127.0.0.1` 临时端口（HTTP 等待超时，连接能力已确认）。独立严格权限 profile（`[permissions.agent_read.filesystem]` 中 `:minimal=read`、`:workspace_roots=read`，`network.enabled=false`）在启动阶段明确失败：`Restricted read-only access requires the elevated Windows sandbox backend`。这代表本机普通只读沙箱**不能阻止越界读取或本机网络连接**，而满足读取隔离的权限配置当前无法启动。保持 `REAL_CLI_SANDBOX_VERIFIED=false`，UI/工具不可启用；未提权、未安装沙箱组件，未访问真实个人文件、激光机和真实模型账号。后续只有在独立授权并验收更强 Windows 后端或独立隔离账号/VM、同时补齐 Windows Job Object 进程树和取消/超时负向测试后才可重新评估开启。测试临时目录已尝试仅针对本轮创建的路径清理。
- **NV13-3 纯 Windows 进程树安全夹具（2026-10-11）**：新增 `crates/laser-host/src/voice_agent/windows_process.rs`（仅内部进程测试框架，未连接生产 `AgentRunner`）。使用 `CreateProcessW(CREATE_SUSPENDED|CREATE_NO_WINDOW|EXTENDED_STARTUPINFO_PRESENT)`、显式仅继承三个标准管道句柄，先 `AssignProcessToJobObject`、复查权限后才 `ResumeThread`，避免子进程先执行再进入 Job。Job 启用 `KILL_ON_JOB_CLOSE | ACTIVE_PROCESS_LIMIT(8)`；超时上限 300 秒，取消、voice permit 撤销与正常结束均通过 Job 终止并等待进程树归零。stdout/stderr 同时排空，单行最多 64 KiB、总量最多 256 KiB，超出仍 drain；任务走 stdin。环境只允许固定 Windows/CLI 引导键，拒绝 MOSS/MCP/云 API key 等；常见 token/password/API key 输出行仅保守遮盖，不保证全部秘密被识别。
- **测试结论**：专用无账号 Rust 进程夹具 10/10 通过，涵盖 Job 关闭、最多 8 进程、超时、取消、权限撤销、创建后恢复前撤销、标准输入、双管道洪泛、有界输出和恶意 env；父子树回收测试另连续 5 次通过。**本机 Codex 0.157.1 仍因越界读和本机 loopback 可达而未通过读取/网络隔离**，严格 profile 缺 elevated Windows sandbox backend。Job Object 不是 OS 文件/网络沙箱，`REAL_CLI_SANDBOX_VERIFIED=false` 不变，生产仍只注册 DisabledRunner；真实 Codex/Claude 模型任务、真实 300 秒压力和完整崩溃场景未验收。
- **NV13-3 阶段完整回归（2026-10-11）**：`cargo test --workspace` **529/529 全部通过**，Windows Job 专项 **10/10**，父子进程取消回收连续 **5 次**通过；前端 `npm test` 与 `npm run build` 通过，FakeGrbl E2E **32/32** 通过。Windows `cargo tauri build` 得到新 EXE **55,533,568 字节**、SHA-256 `ac05a44ebaee92911022e357d22a75f248c84e878343d5afb084441986f8f76c`；普通 NSIS **17,803,006 字节**、SHA-256 `a3916e191f18639238b3420e94c93de78cdcdf095ff27efe21c2981076d74e93`，构建退出码 0，均在该任务启动后更新。Android `cargo tauri android build --target aarch64 --apk` 退出码 0，APK **51,695,818 字节**、SHA-256 `27fbe4d8240effad6d658bfea0a299300d885d2e990a24d40bd79ad2deaba518`；APK 内 arm64 `.so` 与本轮新编译库 SHA-256 一致：`55366fe977983539cf823df1fff238c4e2169a1b6d5b0350bfb9a973335cfb7b`。**普通 NSIS 仍不是 NV18 语音 sidecar 专用安装包**。没有登录真实 Codex、运行模型任务、安装、访问硬件、提交或推送；本机读/网沙箱隔离仍 FAIL-CLOSED。
- **NV13-5 无管理员权限隔离路线验证（2026-10-11）**：仅作只读诊断及临时容器测试。当前 Windows 家庭中文版 25H2、build 26200.9457，终端非管理员，Hypervisor 和 WSL2/Docker 29.8.0 已运行；微软 Windows Sandbox 不支持 Home，但与 Codex 自身 elevated 后端不同，后者未安装/启用。复用本地缓存的 `ubuntu:22.04`，新增可重放的 `scripts/test-nv13-container-isolation.ps1`，强制禁止拉取镜像/网络、不挂载 MOSS/账号文件及 Docker socket，主机仅生成临时 canary，容器只读挂载、无 Linux capabilities、启用 Seccomp、最多 8 进程、256 MiB 内存和 0.5 CPU。测试及脚本回放均通过：只读工作目录、禁止越界宿主机读取、禁止写入、无默认路由、无法连接本轮 Windows 回环监听器、无继承的 MOSS/API 凭据、容器和临时目录清理成功。**不代表真实 Linux Codex 已能运行**：缺受信 CLI Linux 版本、独立认证和受控 API 出站通道，`--network none` 无法访问模型；Windows 原 Codex 越界读/loopback 风险仍在，生产 `DisabledRunner` / `REAL_CLI_SANDBOX_VERIFIED=false` 保持不变。
- **NV13-5 官方 Linux Codex 0.162.1 安装与离线容器验收（2026-10-11）**：用户许可后，从 `github.com/openai/codex` 的 `rust-v0.162.1` 官方发布资产下载 x86_64-musl CLI（111,350,218 字节），与 GitHub API digest 和固定 SHA-256 `86f268d81b898f3e144c5ecff2ad2fda2c5802e042d6195b72538fe0fa125057` 验证一致。使用 `scripts/build-nv13-linux-codex-test-image.ps1` 直接复制已完成 SHA-256 核对的官方二进制，基于缓存 Ubuntu 22.04 离线构建 `moss-nv13-codex-test:0.162.1`（image id `sha256:baa32539eac9ecca7831bac5848c0ecab8a0a84986c5822a4d371fffa7d435ac`，约 526 MB）。没有运行第三方安装脚本、下载新基础镜像或复制账号。`scripts/test-nv13-linux-codex-image.ps1` 已对真实 `codex --version`、`--help`、`exec --help` 以及只读工作区、未挂载宿主机文件、禁写、禁 Docker socket/网络、非 root、8 进程限额、Seccomp 与临时测试容器停止/清理完成 PASS。首次 Dockerfile 格式错误已修复；镜像保留、构建产物临时目录清理完毕。
- **仍未通过的边界**：Codex 自带 `sandbox` 子命令的 Linux 双层沙箱需要 bubblewrap；当前精简镜像中缺系统/bundled bwrap，`--network none`、只读镜像 + 16 MiB 临时 tmpfs 的测试均未能启动内层沙箱，**不得声称双层沙箱 PASS**。没有为此增加容器特权、放宽 Seccomp、安装系统包或调用实际模型；独立登录/模型 API 网络白名单与 NV13 单次审批取消的真实 CLI 集成仍为待办。生产 `DisabledRunner`、`REAL_CLI_SANDBOX_VERIFIED=false` 不变。新增内容仅脚本/文档，未修改桌面代码，不要求本轮重建 Windows/Android App。
- **NV13-5 bubblewrap 供应链 + Linux 命名空间负向验收（2026-10-11，内层 BLOCKED）**：此前真实 Linux Codex 0.162.1 的内置二次 sandbox 缺少 bwrap。本轮只在独立派生测试镜像中安装 Ubuntu 22.04 官方 GPG 签名包 `bubblewrap 0.6.1-1ubuntu0.3`（`moss-nv13-codex-bwrap-test:0.162.1-jammy`，image id `sha256:7e90320dc50afccbd1d036a21e7ee020365182cf599f5b75ac0e839360b150c6`）；先前把 APT 源强制换 HTTPS 因基础镜像缺 CA 证书而失败，随后恢复 Ubuntu 原生 APT 软件源并保留 Release 签名与未认证包拒绝策略，构建成功。`scripts/test-nv13-codex-bwrap-sandbox.ps1` 无凭据测试确认 bwrap 版本正常、`unshare --user --map-root-user` 返回 `Operation not permitted`，真实 `codex sandbox -P :read-only` 返回 `bwrap: No permissions to create new namespace`，**问题仍在 Docker 当前 user namespace 权限，不得强行加 SYS_ADMIN、--privileged 或关闭 Seccomp**。只读合成 canary 未被修改，所有临时容器清理完成。
- **NV13-5 仅假模型代理网络段（2026-10-11，离线 PASS，不等于真实 API）**：增加 Ubuntu 官方 GPG 签名软件源的 netcat-openbsd 测试镜像 `moss-nv13-fake-api-gateway:jammy`（image id `sha256:fed673ea01303bc787097fc8a54ea4ced2a3c886e878978293bd64f7e57d3b6f`）与 `scripts/test-nv13-internal-model-gateway.ps1`，在本机短暂建立 `docker network create --internal`，使用实际 Codex 0.162.1 镜像的 Bash 客户端向**假** HTTP 服务读取纯人工 `NV13_FAKE_MODEL_REPLY_ONLY`，内网可达、无外网默认路由、Windows 仅供测试的回环监听器未收到连接，无容器账号/MOSS 密钥、无其他宿主机挂载、禁写、无 Docker socket、非 root/8 进程/Seccomp 的检查通过。网络/测试文件/容器按唯一名称删除；未对真实 OpenAI 域名/模型发请求。**尚无正式代理的 TLS/域名白名单/认证/撤销/费用限制**，不允许因为假回复成功而启用 Codex。完整隔离和下一步接受标准见 `docs/integration/nv13-codex-docker-security.md`；生产仍为 `REAL_CLI_SANDBOX_VERIFIED=false` 和 `DisabledRunner`，Windows/Android 客户端源码未修改，本轮不重建发布包。
- **NV13-5 离线模型 HTTP 代理请求校验核心（2026-10-11，partial）**：之前的 Docker 假代理只证明内网通信，不验证授权及 HTTP 攻击请求。本轮在 `crates/laser-host/src/voice_agent/model_proxy.rs` 新增无网络上游的 **mock-only** HTTP 路由/权限策略，固定 `POST /v1/responses`、Host、模型及 JSON schema；拒绝任意 URL/CONNECT/重定向式路径、非白名单头、Transfer-Encoding/重复长度、恶意模型/流式/工具字段。使用 OS CSPRNG 一次性 32 字节令牌（内存只存 SHA-256 摘要），绑定 run/device/实时许可，30 秒 TTL、16 个并发令牌上限、原子单次消费，撤销/过期 fail-closed；wire 16 KiB/head 8 KiB/body 4 KiB/回复 8 KiB，有界不含 prompt/密钥的审计，只返回固定人工回复。`cargo test -p laser-host voice_agent::model_proxy --lib` **8/8**、`cargo check -p laser-host` 通过；正式 TLS/CA、DNS/IP 白名单、受控模型 API、密钥和实际费用、Codex 容器与审批/在途取消整合均**未实现**。生产仍 `REAL_CLI_SANDBOX_VERIFIED=false`、`DisabledRunner`，此次只是安全代码骨架和离线测试。
- **NV13-5 本轮完整验收（2026-10-11）**：Rust 工作区 `cargo test --workspace` **537/537 通过**；新增 `voice_agent::model_proxy` **8/8** 通过，`cargo check -p laser-host`、前端 `npm test`/`npm run build` 与 FakeGrbl E2E **32/32** 通过。Windows `cargo tauri build` exit 0，生成 `target/release/moss-desktop.exe`（55,533,568 字节，SHA-256 `51f1ccbc25efb3da74b3b613e6cdd1937d00882c1f72e0a68b203d87c0173344`）和普通 NSIS（17,800,285 字节，SHA-256 `6033a17c5d59d2737ff3f5f8162530075bafdf13f63084173d6d290e51643bb1`），产物更新日期来自本轮构建。Android aarch64 `cargo tauri android build --target aarch64 --apk` exit 0，APK 51,695,818 字节，SHA-256 `1206cd38dc9f214490e0669dcfb7f214b2cf5b8b1e508987713788ffc7f45a4c`；APK 内 Rust arm64 库与本轮产出的 `.so` SHA-256 均为 `03552c2ac928c8f7b46b9496ff56c6567e12fd4446393215750d29aec774f089`。这些均为**软件构建与 mock 测试**，没有安装客户端、调用真实 Codex 模型、修改账号或控制激光设备。普通 NSIS 不等于带语音辅助程序的 NV18 专用安装包；真实外联代理和安全验收仍为 partial。
- **NV13-5 端到端安全代理离线链路 + 固定 Docker 启动计划（2026-10-11，仍 partial）**：先前只有固定假 HTTP handler，缺少真实分段 TCP I/O、TLS 主机名校验和 Docker 启动参数约束。新增 `model_proxy/transport.rs`：2 秒完整请求 deadline、80ms 授权撤销轮询、最多 8 个并发 TCP worker、请求分段/超限/管线重放拒绝、停机 join；`model_proxy/egress.rs`：固定 `POST https://api.openai.com/v1/responses`、443/SNI、拒绝其他 host/路径/方法、私网/环回/云元数据/保留 IP、IPv6 暂拒、超大响应和 302；`model_proxy/tls_client.rs`：reqwest HTTPS-only + 禁环境代理 + 关闭跳转 + 解析结果固定、fake TLS 证书/错误主机名测试、128KiB 限额、异步 25ms 取消轮询；`model_proxy.rs` 将授权单次原子消费与 mock TLS 响应拼成真实 socket→scope→fake TLS→合成结果 E2E，撤销中的等待不得回送结果（上游已收的字节/成本不能撤回）。另 `docker_plan.rs` 固定 SHA-256 镜像、随机合成目录唯一只读挂载、无网络/凭据/特权，单项可执行的 Docker `--version/--help` 计划及实时 Docker 无模型 probe 已通过，修复 Windows `\\?\` canonical path 与 Docker mount 的兼容差异。代理定向测试 **23/23** 和 `DockerProbePlan` 3/3 夹具通过，额外 ignored 实际 Docker 探针按明确 opt-in **1/1** 通过。实际 Linux Codex 模型账号、真实 API、用户文件、MCP 或激光设备均**未使用**；与生产 `AgentCoordinator` 未连通、Codex 内部 bubblewrap namespace 阻塞原样保留。完整 workspace 回归与发布构建结果另记。
- **NV13 断线恢复的完整双端构建回执（2026-10-11）**：Runner 重连后确认旧 Windows release job 结果为 outcome_unknown、无残留构建进程，重新执行 cargo tauri build exit 0：Windows EXE 55,532,544 字节（SHA-256 3aa6a6f0597ccccf8ce604e3bf853666f29fedba94554ad829011990e95ccc9c），普通 NSIS 17,805,005 字节（SHA-256 b62c5daeba793a70a402a804c2709c843c8584c7e32ed33ede82134f9fc948c8）。Android aarch64 cargo tauri android build --target aarch64 --apk exit 0，APK 51,695,818 字节（SHA-256 26873f9df9a97edbb953422df96f76e23c7d53a0a147ada1999b3a836d1401c7），APK 内 Rust arm64 库和本轮编译产物 SHA-256 一致（fc63a3dc3f75a1ee7342592ca45c9ef35694548df00f434c8e48dc6109fe098f）。NV13 Rust 555/555、代理23/23、FakeGrbl E2E32/32、前端测试/构建均通过。这些产物生成在随后 NV14 源码改动之前，不能替代 NV14 最新发布构建。真实 Codex 仍禁用，无模型调用和硬件操作。

- **NV13 本次重连后双端构建复核（2026-10-11，最新产物）**：前一次 release job 因 Runner 重启被标记 `lost/outcome_unknown`，未把该结果计作成功；确认无残留编译进程后重新运行 Windows `cargo tauri build` **exit 0**，最新 `target/release/moss-desktop.exe` **56,871,424 字节**，SHA-256 `d303e7f4f6ae3b14ea34ae0cf696285f77e27123f6082445bc63e61a656dc706`；最新普通 NSIS `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` **18,040,114 字节**，SHA-256 `6e0d7c778df5d56e3a52b3368b4546c0c980c729019dd42c4be75645205d007d`。Android `cargo tauri android build --target aarch64 --apk` **exit 0**，`app-universal-release.apk` **51,734,722 字节**，SHA-256 `ba8a25f67b23b29c662943f4a79aa23be7877e5b326d0492f265abc95dd1ee0b`；APK 内 arm64 `.so` 与本轮 `target/aarch64-linux-android/release/libmoss_desktop_lib.so` 的 SHA-256 **均为 `5d38c0ba787448a885eeb744002263381fcb5f2cc85626614dd8b1674df61727`**。NV13 本阶段 `cargo test --workspace` **555/555**、代理专项 **23/23**、FakeGrbl E2E **32/32**、前端测试与正式 build 均通过（见前阶段回执）；实际 Docker 仅无模型版本探针 1/1。工作区另有 NV14 未提交改动，因此此结果只代表**本次构建时刻源码快照**，不等于 NV14 功能完成或 NV18 专用 sidecar 安装包。生产真实 Codex、实际公网 API 和实机仍未启用/未验证。
- **关联**：crates/laser-{contracts,api,runtime,host}/、apps/desktop/src-tauri/src/voice_builtin.rs、frontend/src/components/VoiceAgentCard.jsx、docs/developer/api-contracts.md、db-schema.md。

### NV12 视觉图片服务：独立模型、强身份校验与设备权限（2026-10-10，软件与双端构建已验证，实机留 FINAL）

- **现象与影响**：此前内置语音无受控图片上传及独立视觉模型，不能将普通 LLM 或设备自称身份用于解码图片。
- **根因**：8003 仅 OTA，DeviceRegistry 无只读、可撤销的 HTTP 授权，MCP initialize 没有视觉能力，桌面也没有独立模型与凭据。
- **修复**：新增独立 OpenAI 兼容非流式视觉提供方（模型最长30秒，响应≤1MiB，禁重定向、密钥不回显）。在 8003 新增 POST /mcp/vision/explain：middleware 在读取图片前验证 Device-Id、Client-Id、Bearer token，并由后台线程只读核对数据库当前 allowed 和授权代次；撤销后即使重新允许也不能复活旧请求。仅支持各一个 question 和 PNG/JPEG image；图片≤5MiB、总请求≤5MiB+64KiB，MIME/魔数一致，解码尺寸≤4096、总像素≤1600万；全局2、同设备1并发，RAII释放；返回前再次核验授权与模型配置。设备 MCP initialize 只有已授权、已开启且模型配置完整才下发实际 HTTP 端口与 token。新增默认关闭的桌面视觉配置和独立密钥（SQLite voice.vision.main），三个只在 main 主窗口可调用的 Tauri 命令以及 React 视觉卡片；连接测试仅发送内存生成的合成图片，不启用摄像头、不授予设备控制权限。Android 配置命令不支持。
- **验证**：视觉 provider 4/4、视觉 HTTP 假设备 5/5、设备 MCP initialize 与桌面私有 ACL 测试通过；Rust 工作区 511/511、前端单测/构建、FakeGrbl E2E 32/32 通过。Windows release EXE（55,296,512 字节，SHA-256 `9b1db6416be366ae4e7f82e22ce2aea0691cdbc25d9d5d87aa6e295f7f176015`）与**普通** NSIS（17,749,922 字节，SHA-256 `8c9b81e6cc24f4406054f25cf696bfd7f0c048462928a270f0ac1b363c17507b`）构建成功。Android aarch64 release 编译完成、Tauri 明确报告 1 个 APK：Gradle 增量复用使 APK 修改时间未更新，额外时间戳断言报错，但 APK 内 `lib/arm64-v8a/libmoss_desktop_lib.so` 与本轮新编译 `.so` SHA-256 **一致**（`cd6bdff0310328efbf70051ba285ee717151729b77045aaa19786ce547669da2`）；APK 51,624,018 字节 SHA-256 `07ca35876506e050fddb37a7a83427fe352b12fbb147794684f4ab9388d8a5ab`。真实固件 multipart、视觉 API、摄像头、端到端性能与 NV18 **专用辅助程序安装包**均留最终验收，不能以普通 NSIS 代替。
- **关联**：crates/moss-voice-providers/src/vision.rs、crates/moss-voice-server/src/vision.rs、crates/moss-voice-server/src/session.rs、apps/desktop/src-tauri/src/voice_builtin.rs、frontend/src/components/VoiceVisionCard.jsx。

### NV11 补齐 NewsNow 与天气/新闻五分钟缓存（2026-10-10，发布构建已通过，真实服务待验收）

- **现象与影响**：此前 `get_news` 只有 RSS，NewsNow 无法配置；天气与新闻每轮请求都会重复访问提供方，易触发限额并浪费网络。
- **根因**：没有 NewsNow 固定 `/api/s?id=` schema 适配、NewsSource 来源类型和隔离的内存缓存。
- **修复**：给 `NewsSource` 增加兼容旧配置的默认 `kind=rss` 和可选 `source_id`；按 NewsNow 已公开的 `status=success/cache`、`id`、`items[].title/url`、`updatedTime` 字段解析，状态/ID/结果字段不符时返回 schema 错误，不把错误当空新闻。桌面增加 RSS/NewsNow 输入格式与来源白名单；NewsNow 支持明确配置的本机/LAN 地址，不接受模型指定 URL。新增仅天气与新闻使用的 5 分钟单飞缓存，最多 32 条成功数据；键涵盖整个设置、请求参数和天气密钥指纹，错误不缓存；调用结束重检设备授权和设置是否已变化，避免旧结果返回。Tavily 与 RAGFlow 不缓存。已保存密钥依旧不出现在工具结果中。
- **验证**：新增 NewsNow schema、缓存新旧键、并发单飞与错误不缓存共 3 项专项测试，NV11 当前累计 14/14 通过；本轮 Rust 工作区首次全量测试中有 1 项原有 NV8a 流式急停时序测试偶发失败（预期文本实际读到音频帧）；该测试单独复跑通过，**完整工作区第二次复测 500/500 全部通过**。此前时序波动仍须后续稳定性跟踪，不能单凭一次通过证明没有竞态。前端单测和构建通过，FakeGrbl E2E 32/32 通过；Windows release EXE（53,672,448 字节，SHA-256 `2c34cf333f909bd19f18ac2c1bc0451841f329aab754116ba4b9c7e14c8fe7e2`）与普通 NSIS（17,347,780 字节，SHA-256 `aa86a116a39dd5c231c13e8454f9252b055a807849628f56e805e2e93552b65b`）有完整构建成功回执；Android aarch64 release APK（51,509,490 字节，SHA-256 `c1101faabc46600843b74b02fbf5237288f634eddb7a590381c5f321f0ed848f`）也明确成功。普通 NSIS 不能替代 NV18 语音助手 sidecar 专用包的安装验收。真实第三方服务及物理设备均未测试。
- **关联**：`crates/moss-voice-server/src/plugins/{newsnow,cache,runtime}.rs`、`crates/moss-voice/src/settings.rs`、`apps/desktop/src-tauri/src/voice_builtin.rs`、`frontend/src/components/VoicePluginsCard.jsx`。

### NV11：信息插件五类软件接线（2026-10-10，软件基础已实现，待剩余适配/真实验收）

- **现象与影响**：内置语音缺少直接查询天气、农历、RSS 新闻、Tavily 与 RAGFlow 的工具，且未经约束的联网请求可能泄露密钥或让外部资料影响设备控制。
- **根因**：没有受控 HTTP 客户端、固定提供方 schema、逐插件默认关闭的配置/凭据入口、实时会话许可复核。
- **修复**：新增 `moss-voice-server::plugins`，包括受控 HTTP（不跳转、3 秒连接/10 秒请求、1MiB 响应、最多 2000 字回填）、和风天气、`lunar_rust` 本地农历、白名单 RSS（禁 DTD/ENTITY）、Tavily、RAGFlow；语音 `CombinedTools` 按配置生成工具，调用前后重读许可/开关，保留旧 `call_raw` 原始急停通路；插件默认全部关闭。新增桌面端 Keep/Set/Clear API 密钥到 SQLite `app_secrets`，非敏感配置到 `voice.builtin_plugins`，`voice_plugins_status/save` 及 React 配置卡，命令 ACL 限主窗口且无 remote。未验证的 NewsNow/Metaso 适配器不暴露；RSS 来源需配置 HTTPS 白名单且不能含 query；RAG 数据集取配置不能由模型改变。禁原始响应和密钥日志。用户开启后的搜索/天气/RAG 请求会发送到配置的服务商，可能产生费用。
- **验证**：NV11 11 个单元测试含春节/闰月、RSS 数据、模拟 HTTP 401/429/302/超大包、撤销、工具清单和原急停 `call_raw` 保持；本轮 Rust workspace **497/497** 通过、前端单测/构建通过、FakeGrbl E2E **32/32** 通过；Android aarch64 release 构建明确成功并生成 APK（51,508,834 字节，SHA-256 `831a20f6361dc9275707382cc058bb570c95061439c12826c8ef4760108934f1`）；Windows EXE 更新（53,600,256 字节，SHA-256 `e2cd44583af985a364e912b99adbb90da965e0e5ba7af794ca10651a8000ea5a`），NSIS 本轮生成更新文件（17,335,572 字节，SHA-256 `443b44bb9fa0433bf0e2e44aafe5a2fd4d7478b00eafed0a5aa359726e607a00`），但 Windows 构建任务状态丢失，正式完成回执未取得，**不能判完整发布验收通过**。本 NSIS 为默认构建，不是 NV18 辅助程序专用安装包。**未完成**：NewsNow 的版本化 fixture 与适配、完整提供方请求/响应模拟、天气/新闻五分钟缓存、真实第三方连接/数据质量及真实 UI 操作，NV11 整体保留 `partial`。
- **关联**：`crates/moss-voice-server/src/plugins/`、`crates/moss-voice/src/settings.rs`、`apps/desktop/src-tauri/src/voice_builtin.rs`、`capabilities/voice-plugins.json`、`frontend/src/components/VoicePluginsCard.jsx`、`docs/developer/api-contracts.md`。

### NV10-2～4：按设备隔离的可选长期语音摘要（2026-10-10，软件构建已通过，真人/GUI 验收待最终阶段）

- **现象与影响**：原有 schema v7 虽能保存摘要，却未在会话结束后生成/注入，也没有管理开关和界面；若无并发锁，清空后旧任务可能写回。
- **根因**：未实现服务层、桌面 DbRegistry 适配和桌面交互接口。
- **修复**：新增 `moss-voice-server::memory::MemoryService`，默认关闭，由用户开启后在会话中加载同一设备的摘要作为不可信历史数据。断线后的已提交对话有界总结（最多 6000 字输入/1500 字摘要），最多 2 个执行任务及 16 个等待名额；与开关、编辑、撤销共享协调锁和数据库 revision 防旧结果覆盖。新增 Tauri 桌面记忆列表/开关/编辑/清空命令及 UI。只传经过筛选的用户/助手文字给当前配置的 LLM，不存原始录音，不赋予工具权限。Android 命令返回不可用。
- **验证**：后端三项 `memory::tests` 单测通过（默认关闭/设备隔离、清空/关闭/撤销竞态、工具与凭据过滤），桌面独立本地主窗口 capability 权限单测通过；本轮 Rust 工作区 485/485 项通过、前端单测/构建及 FakeGrbl E2E 32/32 通过；Android aarch64 release APK 明确成功并生成新文件。Windows 首次构建任务中断后，已从缓存增量构建成功，生成新的 release EXE 和 NSIS；Android aarch64 release APK 也已构建成功。三个本轮产物 SHA-256：Windows EXE `e2703928f216e90ddac92f2d33c7248d43345344d7fd589928f6b885d279c834`（52,891,648 字节）；Windows 默认 NSIS `44049691f711158bc6e9dbcccdad70e7e7fb2102171b126e610fe95d377f3b0c`（17,173,501 字节）；Android APK `e3de21048bc2644cb0912360ca242f9386053473019307bf0e1dad4373a4833f`（51,433,962 字节）。**默认 NSIS 不是 NV18 sidecar 专用构建，不能以本次成功代替 NV18 的 helper 安装验收。**真人设备、总结质量、云模型供应方隐私边界保留 FINAL 统一验收。功能在全部构建和安全验证完成前仍为 `partial`。
- **关联**：`crates/moss-voice-server/src/{memory,session,conversation,lib}.rs`、`crates/moss-voice/src/settings.rs`、`apps/desktop/src-tauri/src/voice_builtin.rs`、`frontend/src/components/VoiceMemoryCard.jsx`，并同步 Tauri 命令/权限表、`voiceApi.js`。敏感记忆管理权限从含 localhost 远程授权的通用 capability 拆出，仅在无 remote URL 的 `voice-memory.json` 中允许 `main` 窗口；相关负向 ACL 单测已通过。

### NV10-1：设备记忆 schema v7 与防旧总结覆盖接口（2026-10-10）

- **现象与影响**：内置语音尚没有可持久保存和用户控制的设备级摘要；异步总结若没有条件更新，用户清空后旧任务可能把已删除内容写回来。
- **根因**：RuntimeDb 原最高 schema v6 仅有设备登记表，不含摘要 revision/更新时间和乐观锁写入接口。
- **修复**：单事务迁移到 v7，新增 `voice_memory(device_id,summary,revision,updated_at)`；`ensure_voice_memory` 幂等首次初始化、只读查询/列表及 `update_voice_memory_if_revision` 条件更新，最长 1500 Unicode 字符，清空也会递增 revision；返回 None 表示有并发编辑，不允许旧摘要强制覆盖。
- **验证**：NV10 两项 SQLite 单测通过；`cargo test --workspace` 全部 481 项通过、前端单测 102 项、E2E 32 项通过（FakeGrbl）、前端构建通过。**这只是 NV10-1 的持久化基础**：内置语音总结/注入、设备权限和开关、界面增删、真实模型总结与实机均未接线，不能称完整 NV10。Windows 与 Android 本轮交付构建以实际验证记录为准。
- **关联**：`crates/laser-runtime/src/{db,lib}.rs`、`docs/developer/db-schema.md`、`docs/integration/moss-fusion-implementation-todo.md`。向后兼容需评估旧客户端对 v7 的支持；软件回滚须恢复兼容数据库备份。

### 修复：Windows 内置语音辅助程序打包资源与 NV8a-1 中间识别急停（2026-10-10）

- **现象与影响**：NV18 的 NSIS 专用构建只复制 helper EXE，缺少 GPL 对应源码/许可资源与交付清单；NV8a 原实现等最终 ASR 文本才急停，延迟于已确定的中间结果整句立即停机规则。
- **根因**：Tauri sidecar overlay 只有 `externalBin`，构建脚本未准备资源树；会话 `select!` 只处理音频/设备/VAD/最终识别，没有等待 `AsrStream::next_interim`。
- **修复**：脚本统一独立 helper 与主应用的 `target` 构建目录、清除旧 sidecar 后构建、收集 `moss-voice-local` 与其 path 依赖源文件/COPYING/初始第三方说明，新增 Tauri `bundle.resources` 和构建产物 SHA-256 清单；语音会话监听完整中间假设，命中现有精确停止短语就切为单次 `FastStop`，不再等待最终文本。失败/撤销时不以软件触发证明机器已经停稳。
- **验证**：针对性 Rust 测试 `interim_whole_stop_executes_without_waiting_for_final_and_once` 通过（1/1，`FakeTools`）；NSIS 实际构建、资源/许可完整性、三端重建和实机测试以本轮后续实际记录为准，缺项仍为 `partial/unverified`。流式 ASR 中间结果可能只是暂态前缀，“停止之后怎么办”若先被识别为单独的“停止”，也会提前停机；这是选定低延迟策略的已知误触发风险，不能宣称彻底消除。
- **关联**：`scripts/build-windows-installer.ps1`、`apps/desktop/src-tauri/tauri.voice-sidecar.conf.json`、`apps/moss-voice-local/THIRD_PARTY_NOTICES.md`、`crates/moss-voice-server/src/{session.rs,tests/chat.rs}`、`docs/integration/moss-native-voice-implementation.md`。

### 修复：SD 授权竞态、跨页面急停、未确认停止、语音绑定、管道阻塞及安装包辅助程序（2026-10-10）

- **现象与影响**：语音授权在 SD 上传期间撤销后仍可能启动加工；离开发送页面后顶栏急停失效；通信失败后任务从活动表移除，无法按原编号重试 0x18；伪造同 Device-Id 的客户端可降级已配对设备并撤销正常会话；辅助程序 stdin 堵塞可卡住调用线程并堆积关闭线程；常规安装包未带本地语音程序。
- **根因与修复**：SD 启动前复核取消和语音许可；新增后端全局精确停止目标查询及前端单目标急停，多目标绝不猜测；非完成任务保留原实时传输通道直到该设备重新通过预检，支持失败后的 0x18 重试；不允许不同 Client-Id 的匿名来访篡改现有数据库绑定；辅助程序改为单写入线程、有界 16 帧队列、请求总时限与异常进程终止，取消流不再新建线程；增加 Windows helper 独立 release 构建与可选 Tauri sidecar 安装包脚本 `scripts/build-windows-installer.ps1`。
- **验证范围**：新增前端跨页面、单任务/多任务及失败重试逻辑 4 个回归测试；调整 RuntimeDb 不可信身份冲突测试及本地 VAD 超时失效测试。软件检查结果以本次实际执行为准；实体急停、物理设备状态、安装包全新安装与真实语音尚未验证。
- **限制**：0x18 写出不是物理停稳的证明。旧失败任务通道仅用于精确 job_id 停止，重新通过设备预检后清除；出错或多目标状态请使用实体急停。Windows 带 helper 安装包必须经专用脚本构建，常规 `cargo tauri build` 不会自动生成或捆绑独立 GPL 辅助程序；发布前须履行 GPL 相关源代码和许可义务。
- **关联**：`crates/laser-api/src/lib.rs`、`crates/laser-runtime/src/{db,lib}.rs`、`crates/moss-voice-providers/src/local*`、`frontend/src/{main.jsx,globalEstop.js}`、`frontend/tests/globalEstop.test.mjs`、`apps/desktop/src-tauri/tauri.voice-sidecar.conf.json`、`scripts/build-windows-installer.ps1`。


### 新增：NV9 四段接入——桌面配置、连接维护、持久审批与审计、语音工具及统计（2026-10-10）

- **现象与影响**：NV9 之前只有客户端基础库，界面不能添加第三方服务，语音不能调用，缺少连接维护和外部操作的持久审批／审计。
- **根因**：客户端未接入桌面生命周期、settings/app_secrets、ApiService 审批及 HostTools 语音目录；原审批目标只表示激光设备。
- **实现**：新增 ExternalMcp / Manager、桌面主窗口专用命令与来源核实、版本化凭据引用及配置版本校验；普通语音设置不覆盖 MCP 配置。服务默认停用，工具默认每次批准；独立心跳/工具刷新、1–60 秒重连、不补发旧操作。最多 8 服务，每服务发现上限 256、全局模型工具上限 64、描述 200 字符；列表与调用均拦截停用、冲突和超预算项。只有已允许的内置语音 device/session 可调用，Ask 不随激光严格模式关闭。
- **审批与故障**：external_tool 以服务、连接／凭据、schema、权限、来源和完整参数绑定摘要，批准与下游执行前复核；单次消费并在同一事务记运行态，改配置／撤销后旧审批失效。执行前审计失败不调用，执行后结果／审计失败明确报告可能已执行，不自动重试；重启恢复未知结果。参数预览有界脱敏，完整参数仅留本机隐藏 request，审计不记参数、结果和凭据。
- **页面**：添加/编辑/移除服务及凭据、三档权限、连接/失败/重连状态、内存调用次数与最近时间、持久审计。审批卡改为核对第三方参数和数据范围；待审批语音提示去电脑批准，批准后的结果在桌面查看，本期不主动回推语音。官方小智桥不会自动开放外部工具。
- **默认、兼容和边界**：支持 stdio 与 Streamable HTTP，不支持旧独立 SSE 或自动 OAuth；本机程序不是沙箱。表 schema 仍为 6，但新审批／审计枚举不兼容旧应用，回退需升级前备份。崩溃可能残留不可达凭据引用，未全库清除。本次不修改辅助程序，协议仍为 3；辅助程序安装包集成缺项仍未解决。
- **验证（最终，2026-10-10）**：`cargo test --workspace --no-fail-fast` 478 项通过；`npm test` 98/98；浏览器假桥 1280/960 像素的添加、凭据分离、空白保留、HTTPS 限制、策略确认、重连、移除、统计／审计和无横向溢出通过。客户端/管理器 14 项、宿主 NV9 9 项及桌面边界已计入全量。`npm run build`、`cargo tauri build`、`cargo tauri android build --target aarch64 --apk` 和 release `moss-host` 均成功；使用新宿主的回环 E2E 32/32（模拟器 8 次连接、3802 行、2 次 0x18），测试后端已回收。真实服务／账号／真人语音未验证；未安装、部署或操作硬件。
- **实际产物（项目相对路径，已检查非空、更新时间与 SHA-256）**：`target/release/moss-desktop.exe`（52,509,184 字节，UTC 10:37:20，`2ab7e276421331cdd50da7d05467d55523ca01bfa81a46889441c00042c095b9`）；`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe`（17,102,324 字节，UTC 10:37:20，`e7d7e098063b229810445f947b8e0773d95f6701f81b05faec23ba7c4a28950d`）；`target/release/moss-host.exe`（26,006,016 字节，UTC 10:39:28，`c65a6e3a37cd9486af8861b119b7c759b729f87d89a126b12f9f47745cc538e0`）；`apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk`（51,298,306 字节，UTC 10:39:41，`5fa4e115812f8cf70978a7bfb056b466e0084408648c78c654319afbc7a86dc5`）。构建保留既有链接器、前端分包体积和 Gradle 弃用提示，不冒称零警告。
- **未完成的文档项**：既有 `xiaozhi-prompts/latest.md` 混合换行使结构化编辑拒绝；后续只读检查又被工具安全检查拦截，未绕过。该提示词及对应新日期归档保持未更新；NV9 的实际行为、安全门禁和使用契约已记录在独立技术文档，不依赖提示词授权。浏览器截图已生成，但 artifact 图像读取受策略限制，未宣称人工视觉验收。
- **关联**：`crates/moss-mcp-client/{src,tests}`、`crates/laser-host/src/external_mcp*`、`crates/laser-api/src/{external_tools,approval,lib}.rs`、`crates/laser-contracts/src/approval.rs`、`crates/moss-voice/src/settings.rs`、`apps/desktop/src-tauri/src/voice_mcp.rs` 及专用 capability、`frontend/src/{voiceMcpState.js,components/VoiceMcpCard.jsx,components/ApprovalCenter.jsx}`；[第三方 MCP 契约](docs/integration/moss-external-mcp.md)、技术书 §13.13、实施 TODO。


### 修复：撤销语音授权立即失效、停止通道与状态监控保持可用、审批不再跨来源复用（2026-10-10）

- **现象与影响**：撤销只改数据库，旧连接仍能调用；无活动任务的停止依赖确认字段；轮询失败导致假终态并移除停止入口；识别中间词可能把询问误当停止；同步本地判停阻塞语音事件循环；不同调用方的相同请求编号复用旧审批。
- **根因**：授权仅在握手校验；连接和执行缺少共享的失效标记；通信状态混入任务状态；中间识别没有语句结束依据；同步 IPC 放在异步事件循环；审批唯一键没有可信来源作用域，也未核对重复请求内容。
- **修复**：①注册当前客户端与会话的失效标记，撤销清理所有旧连接权限并使该设备未消费的审批过期；执行入口、创建审批和发送前/排队发送开始再次检查，重新允许不恢复旧标记。②保留已补齐的 confirmed 停止调用，新增无任务实际字节测试；多任务无明确目标时拒绝随意选取；job_id 只用于查询/停止，不可改写其他动作的授权目标。③审批、工作台与标定轮询改为单请求、有超时、可取消与迟到结果隔离；失败只显示状态未知并继续查询，审批与工作台覆盖层保留急停；取消/急停回复不再冒充物理停机。④停止只在最终识别的独立短句命中后走本地通道，中间结果不产生设备动作。⑤本地判停放入独立线程；输入/结果队列各 64 项、最多 16 个线程、每项排队至回复总时限 1.5 秒，旧拾音结果不进入新一轮；超限/超时结束异常会话，不占住其他设备控制循环。⑥请求编号按可信调用方与会话生成 scope-v1 哈希；相同作用域编号但目标/参数不同返回 409 approval_request_conflict，不恢复已结束审批。
- **默认值、契约与升级**：严格模式、设备地址、模型设置不变；停止不再使用中间词提前触发，是有意的行为变更，真人延迟需实测。本次未升级数据库 schema，原记录保留；新审批 request_id 为作用域哈希，不再等于原始编号。没有可验证独立调用方身份的旧 McpBridge 不按自报 request_id 复用审批，需要分别批准。voice_session_revoked 拒绝旧会话；请求编号超过 128 字符拒绝而不是截短。本地辅助程序协议仍为 3（沿用 NV8a，本次没有修改辅助程序）。
- **验证（2026-10-10：软件通过，实机 unverified）**：全量 Rust 462 项通过，6 项显式忽略的测试未执行；`npm test` 95/95；重建 release `moss-host` 后 `npm run test:e2e` 32/32（仅回环 GRBL 模拟器，观察到 3859 行、4 次进给保持、2 次 0x18）；`npm run build`、`cargo tauri build`、`cargo tauri android build --target aarch64 --apk` 均成功。新增回归覆盖 `revoked_sessions_cannot_return_after_allow_and_other_devices_survive`、`revoke_disconnects_all_live_sessions_without_restarting_server`、`revoke_during_preflight_prevents_queued_gcode_from_starting`、`revoke_during_connect_prevents_safe_motion_and_framing`、`voice_stop_without_active_job_writes_reset_and_reports_connection_failure`、`emergency_stop_targets_only_requested_job_and_dry_run_never_writes`、`interim_prefix_never_executes_and_final_stop_executes_once`、`stalled_local_vad_does_not_block_two_devices_ping_or_revoke`、有界积压/旧轮次丢弃、审批来源隔离/冲突/过期，以及实际覆盖层渲染和轮询断线恢复。相关 tracked diff 的 `git diff --check` 通过。
- **边界**：撤销不撤回已经发送给机器的指令；0x18 写入成功不证明物理停稳；本地识别不可用或设备连接断开时不能保证语音停止送达，必须保留实体急停。硬件验收仍为 unverified。未提交、推送、安装、发布或刷机。
- **本次产物**：`frontend/dist`、`target/release/moss-host.exe`、`target/release/moss-desktop.exe`、`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe`、`apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk`；均已核对非空、修改时间与 SHA-256。桌面程序 SHA-256 为 `6a771667c225e98c9e87ae5f2230b5e3438c7f0fbeb4dc3c3164f81908f558fc`，安装包为 `1d280b9e3ca4904075add5833e74a8f0db5d1adbabd46baf45b79d2ea226cec4`，APK 为 `5459ce6741efca6289a072f9735118e9b294031fe0db31be96085b36706eb0a9`。
- **保留事项**：前端现有大分块/Radix 指令警告、Windows 链接器提示、Android 现有 Gradle 弃用与路径检查警告均未通过放宽检查消除。项目整理检查仍提示原有 `.birdview/jev-secret.activity.html` 与 `.birdview/jev-secret.activity.jsonl` 名称疑似含密钥且未被忽略；本次未读取内容、未删除或自动改忽略规则，不宣称工作区可无检查地公开提交。新增回归测试是长期保留源码，不作为临时文件删除。
- **关联**：`crates/laser-api/src/{voice_authority.rs,approval.rs,lib.rs}`、`crates/laser-runtime/src/db.rs`、`crates/laser-host/src/lib.rs`、`crates/laser-tools/src/lib.rs`、`crates/moss-voice-server/src/{lib.rs,session.rs,audio/worker.rs,tests.rs,tests/chat.rs}`、`apps/desktop/src-tauri/src/voice_builtin.rs`、`frontend/src/{approvalJobMonitor.js,approvalState.js,api/client.js,components/ApprovalCenter.jsx,components/SendOverlay.jsx,pages/HomePage.jsx,pages/MaterialLabPage.jsx}`；安全、API、前端行为和内置语音技术书。


### 新增与修复：NV8a 停止快捷通道、热词与分段合成；实时急停写入失败不再误报成功（2026-10-10）

- **现象与影响**：停止短句和常用只读查询原先需要等待最终识别及大模型；本地合成等待整段音频，缺乏分阶段延迟诊断。活动作业的急停传输失败时，取消标志已置位仍可能被当成停止成功。
- **根因**：会话链路缺少大模型前的固定路由、流式中间文字通知和分段音频协议；急停写入错误被忽略，取消请求与设备响应混为一谈。
- **修复**：精确停止短句在大模型之前执行，流式中间结果每轮最多触发一次；固定停止调用补齐现有工具契约的 confirmed 参数，不放宽其他动作门禁。常用只读问题调用现有工具，不编造物理加工进度或剩余时间。实时 0x18 写入失败返回失败，成功也只表示软件请求已发送，不证明机器物理停止。
- **默认值与契约**：`voice.builtin_local` 新增 `read_only_shortcuts=true`、`asr_beam_search=true`、`custom_hotwords=[]`；热词去重后最多 64 条、每条 32 字符、共 4096 字节，拒绝控制字符。X-ASR 支持 beam search 与可表达的热词；SenseVoice 不宣称支持此通路。云端识别仍依赖网络，离线停止须具备可用的本地识别链路。
- **破坏性升级**：本地辅助程序协议由 2 升为 3，增加解码选项、中间文字及分段合成音频；必须同时更新桌面与 `moss-voice-local`，旧辅助程序会被握手拒绝。分段返回不等于模型内部逐采样推理；取消会丢弃该轮响应，不保证抢占已开始的同步推理。辅助程序纳入安装包仍属于 NV18，不能把主安装包等同于包含辅助程序。
- **诊断与跨端**：语音中心显示最近 20 轮内存耗时；复制仅取允许字段，不复制完整对话、音频、凭据和地址。语音管理仍为桌面功能，Android 沿用现有远程模式。
- **验证**：见下方本次联合验证；覆盖固定停止通道、中间结果只执行一次、`active_job_stop_write_failure_is_not_reported_as_success`、热词规范化与请求字段、诊断复制白名单。未跑真人或硬件测试，HW-09 保持待测。
- **关联**：`crates/moss-voice-server/src/fast.rs`、`session.rs`、`crates/laser-api/src/lib.rs`、`crates/moss-voice-providers`、`crates/moss-voice-local-proto`、`apps/moss-voice-local`、`frontend/src/voiceState.js`、`frontend/src/components/VoiceBuiltinCard.jsx`；内置语音技术书 §13.11。

### 新增：NV8b 自然插话保留句首，取消后不把未开始播放的尾句写入历史（2026-10-10）

- **现象与影响**：过去依赖唤醒词或按钮打断，播报期间上行音频不能直接转为下一轮；生成快于播放时，未听到的回答可能进入对话历史。
- **根因**：没有独立插话判停与句首缓冲，完整生成结果早于播放完成提交，设备自动重开监听还可能清空插话开头。
- **修复**：为 realtime 上行增加独立较严格的连续语音检测，保留最多一秒开头，触发后清空旧播放队列并交给下一轮识别；重复的自动监听开始事件不覆盖该缓冲。首帧发出后保护 300 毫秒；问候与退出告别不参加自然插话。中断历史仅保留已发出 sentence_start 的句子，完整历史等正常播完再提交。
- **默认值与安全边界**：`barge_in=false`、`barge_in_ms=400`，前后端限制 300–600 毫秒整数。依赖设备端 AEC 与 realtime 上行；服务端检测不是回声消除。本次未改固件、未刷机、未接触真实设备。
- **验证**：7 项专项测试通过，覆盖连续语音与噪声、缓冲上限、思考阶段打断、问候/告别保护、句首回送、历史裁剪、关闭与非 realtime 模式；已纳入工作区回归。真人插话延迟、实际回声与噪声误触发率仍按 HW-10 待验收。
- **关联**：`crates/moss-voice-server/src/barge.rs`、`session.rs`、`tests/chat/barge.rs`、桌面内置语音启动配置、前端本地语音设置；内置语音技术书 §13.12。

### 进行中：NV9 第三方 MCP 客户端基础库，尚未接入桌面与语音工具（2026-10-10）

- **背景与实现**：新增 workspace 成员 `crates/moss-mcp-client`，锁定 rmcp 3.5.1，提供 stdio 与 Streamable HTTP、显式连接/断开、有界工具发现、服务器工具别名、凭据引用、逐工具权限、内存统计以及心跳/重连 API。
- **权限边界**：服务默认停用，工具缺省 Ask；默认审批器只返回待审批，不执行。模型自填 confirmed / approval_id 和服务端只读提示都不代替授权。库验证批准摘要与当前调用匹配，但真正的持久审批、一次性消费和审计仍须由后续宿主实现。
- **异常语义**：工具调用超时或连接失败视为结果未知，不自动重放；显式重连不补发上一调用。当前没有驱动心跳和退避的宿主维护任务，不能称为应用已自动重连。
- **验证**：9 项测试全部使用回环 HTTP 假服务或仓库内 Node stdio 夹具，覆盖握手、环境隔离、默认审批/伪造摘要拒执行、禁用检查、目录边界、401/404/500 不重复调用、显式重连与统计保留。未连接真实第三方服务或读取其账号。
- **仍未完成**：桌面服务配置与凭据保存、连接维护任务、跨服务器工具预算及冲突提示、external_tool 持久审批与审计、语音工具聚合和统计页面。NV9 状态为 partial，不是可用的桌面功能；续做清单在实施 TODO 与技术书 §13.13。

#### 本次联合验证与交付（2026-10-10）

- `cargo test --workspace --no-fail-fast`：451 项通过；`npm test`：90/90；`npm run test:e2e`：32/32。E2E 使用新构建的 release 宿主、临时运行目录与回环 GRBL 模拟器；验证 3935 行模拟 G-code、4 次进给保持与 2 次软复位字节，测试进程已回收。真实模型/硬件相关的忽略测试不计为本次实机验证。
- `npm run build`、`cargo tauri build`、`cargo tauri android build --target aarch64 --apk`、独立辅助程序 release 构建均成功。`moss-voice-local.exe --version` 确认 protocol 3。已核对文件非空、更新时间并计算 SHA-256。
- 本次产物：`target/release/moss-desktop.exe`、`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe`、`target/release/moss-voice-local.exe`、`apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk`。
- 构建仍有前端大 chunk / Radix 指令提示、辅助程序 LNK4098 默认库提示和 Gradle 弃用提示；未把它们隐藏或解释成无警告。未安装、发布、刷固件或操作真实设备；NV8a / NV8b 实机验收为 unverified，NV9 应用集成为 partial。

### 新增：内置语音本地模型可选——判停、识别、合成各有多个模型，附优缺点；本地流式识别；自动拾音保留开头改为 1 秒

- **现象与影响**：NV6 的本地模型是固定的三个（Silero、SenseVoice 2024、Kokoro v1.0），用户不能按电脑性能或需求换用更快、更小或音色更多的模型；本地识别只能说完后整段识别，说完到回答约 1.1 秒。另外自动拾音只保留开口前 300 ms，而本地判停实测要在开口后 0.4–0.9 秒才确认“开始说话”，句首一两个字可能丢失（端到端测试中“帮我看看激光机的状态”被识别成“光机的状态”）。
- **根因**：模型清单、辅助程序协议与设置都只有一个固定模型，辅助程序只会加载 SenseVoice / Kokoro / Silero；本地识别没有实现 NV7 的 `StreamingAsr`；`PREROLL_MS = 300` 是按能量判停（几十毫秒确认）定的，没有考虑 Silero / TEN-VAD 要先攒够最短语音时长才报开始。
- **修复**：
  - 模型清单（`moss_voice::local_models`）：新增 TEN-VAD、SenseVoice 2025、X-ASR 流式识别（480 ms 块、中英、带标点、int8）、Kokoro v1.1 int8、Matcha 中英，共 8 项；每项有类别、引擎、一句优缺点（速度为本机实测）和多音色模型的音色范围（Kokoro v1.1：3–57 中文女声、58–102 中文男声，按合成语音基频实测）。Matcha 需要另一个发布标签里的声码器，作为附加文件与压缩包一起下载、各自校验 SHA-256，任何一个不符都不安装。
  - 辅助程序协议升为 2：加载请求带引擎；新增流式识别 `asr_open` / `asr_feed` / `asr_finish` / `asr_close`。辅助程序支持 TEN-VAD、流式 Zipformer（也可整段识别作为退路）、Kokoro int8 模型文件、Matcha（声学模型 + vocos 声码器），有中文读法规则文件时启用；流式识别说完补 1.0 秒静音（0.66 秒实测会丢末字）。
  - 客户端新增 `LocalStreamingAsr`（实现 `StreamingAsr`，打断时关闭辅助程序里的这一路）；桌面端选用流式模型时对话同时挂上它，整段识别作为退路。
  - TEN-VAD 经设备的 Opus 编解码后对短词不敏感（默认参数下“停止”完全检测不到、长句晚 1.0–1.8 秒），改用最短语音 100 ms（`VadParams::for_engine`）；代价是突发白噪声的开头偶尔被当成一小段语音，已写进界面介绍，推荐 Silero。
  - 会话 `PREROLL_MS` 300 → 1000（对所有判停生效）。本地判停出错的事件文字不再写死“Silero”。
  - 设置 `voice.builtin_local` 新增 `vad_model` / `asr_model` / `tts_model`；分区是否可用改看选中的模型；`voice_local_save` 校验模型 id 与类别。
  - 界面：本地模型按判停 / 识别 / 合成三张表分组，每类一个开关，每行可选用并显示优缺点、大小、状态与操作；音色说明与范围随所选合成模型变化，改选多音色模型时填入其默认音色，单音色模型提示“只有一个音色”；下载失败的原因改为标签下方的小字。
- **默认、契约与升级**：默认模型不变（Silero、SenseVoice 2024、Kokoro v1.0），NV6 保存的设置按原模型读取。**辅助程序与 MOSS 必须一起更新**：协议号 1 → 2，旧的 `moss-voice-local.exe` 配新 MOSS 会在握手时报“版本不匹配”，需按 `apps/moss-voice-local/README.md` 重新构建。状态 `local.models[]` 新增 `kind`、`engine`、`note`、`streaming`、`voices`，`size` 改为含附加文件的下载大小；失败状态的界面文字改为“下载失败”+原因。自动拾音的识别音频开头多带最多 0.7 秒（云端整段识别多传这部分）。模型不随 MOSS 分发，许可证见 `THIRD_PARTY_NOTICES.md`（TEN-VAD 带附加条款）。
- **验证（2026-10-08，partial）**：新增自动测试——协议 1 项、客户端 3 项（假辅助程序的流式识别、丢弃后关闭、打开失败）、模型管理 1 项（附加文件下载与校验）并扩充清单一致性检查、设置 2 处、前端 1 项；会话测试按 1 秒保留量更新（语音前 1500 ms 静音只留最后 1000 ms）。`cargo test --workspace` 全部通过；`npm test` 87/87；预览桩走查（分组、选用、改选合成模型换默认音色、音色范围报错、单音色提示、提交内容、窄窗口无横向滚动）。真实模型（`#[ignore]`，本机）：六个新文件下载后 SHA-256 与官方摘要一致；`real_optional_models` 通过（SenseVoice 2025、X-ASR 识别回原句，X-ASR 说完到出字 50–100 ms，Matcha 合成 1 秒语音约 0.04 秒，Kokoro v1.1 int8 约 1 秒）；`real_local_fast_models_end_to_end`（TEN-VAD + X-ASR 流式 + Matcha，上行经 Opus）连续 4 次识别完整、首句延迟 253–315 ms；`real_local_models_end_to_end`（NV6 组合）首句延迟 1113 ms；`real_helper_smoke` 通过。E2E（重新构建 release `moss-host`，本地回环 GRBL 模拟器）32/32；`npm run build` 成功；辅助程序 `cargo build --release --manifest-path apps/moss-voice-local/Cargo.toml --target-dir target` 成功（`target/release/moss-voice-local.exe`）；`cargo tauri build` 成功（`target/release/moss-desktop.exe`、`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe`）；`cargo tauri android build --target aarch64 --apk` 成功（`apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk`）。未验收：真实设备与真人语音（以上均为合成语音）、其他电脑上的速度、安装包包含辅助程序（NV18）。
- **关联**：`crates/moss-voice-local-proto/src/lib.rs`、`apps/moss-voice-local/{src/main.rs,README.md}`、`crates/moss-voice-providers/src/local.rs`、`crates/moss-voice-providers/src/local/tests.rs`、`crates/moss-voice/src/{local_models.rs,settings.rs}`、`crates/moss-voice-server/src/{session.rs,tests.rs,tests/chat.rs,audio/vad.rs}`、`apps/desktop/src-tauri/src/voice_builtin.rs`、`frontend/src/{voiceState.js,styles.css,components/VoiceBuiltinCard.jsx}`、`frontend/tests/voiceState.test.mjs`、`THIRD_PARTY_NOTICES.md`；内置语音技术书 §13.10；实施清单进度记录。

### 新增：小智接入点增强——多接入点、心跳断线判定、工具开关与调用统计、接入点身份（NV8）

- **现象与影响**：MOSS 只能连一个小智接入点，想让多个智能体（或多个账号）同时使用 MOSS 的工具做不到；网络中途断开而 TCP 不报错时（如路由器掉线、休眠唤醒），桥会一直以为在线，小智调用工具全部失败且不会重连；连接稳定运行后出错，重连等待沿用之前加倍后的时间；工具无法按需关闭——不想让语音使用的工具（如材料库）只能靠严格模式整体限制；也看不到各工具被调用了多少次、失败多少次，以及接入点属于哪个智能体、token 何时过期。
- **根因**：`run_bridge` 只接受一个地址、遥测只有一个全局 `connected`；读循环只靠 500 ms 读超时检查退出，没有心跳与空闲判定；退避只在“正常关闭”时归零；工具清单与分发只看严格模式，没有用户开关；调用不计数；接入点 token 只做脱敏显示。参考的 xiaozhi-client 虽有多接入点与开关，但读代码核实其开关不生效（禁用的工具仍上报、调用不检查）且断线不重连（技术书 §2.3），不能照搬。
- **修复**：
  - `laser-contracts`：遥测新增每个接入点的状态 `EndpointLink`（编号、脱敏地址、是否连接、本次连接时间、累计连接次数、上次断开原因、对方客户端）与工具调用统计 `ToolCallStat`（调用、失败、拒绝次数，最近时间与调用方）；总 `connected` 改为“任一接入点已连接”，全部断开才清空客户端标识。不能关闭的工具 `ALWAYS_ENABLED_TOOLS`（`laser_safe_action_tool`，语音“停”依赖它）与 `tool_can_be_disabled` 放在分级模块。
  - `laser-mcp`：`run_bridge(link_id, …)` 每个接入点一条；每 30 秒发 WebSocket Ping，90 秒收不到任何数据（含 Pong）即判定断线重连；连接稳定 60 秒以上后断开，等待从 1 秒重新开始；断开原因去掉 token 后记入该接入点。`HostBackend::tool_disabled`：关闭的工具不出现在 `tools/list`（`tools_list_with`），调用返回 `tool_disabled` 且不触达后端，停止类工具不受开关影响。`dispatch_tool_call_with` 对每次调用按“成功 / 失败 / 拒绝”计数（开关与安全分级拒绝计为拒绝，不算失败；未分级的工具名统一计为“未知工具”，避免接入点发来的任意名字撑大统计）。`endpoint_identity` 解码 token 中 JWT 的载荷，只取标识类字段（键名以 id 结尾或 purpose / name / sub 等）与签发、过期时间，不校验签名、只用于显示。
  - `laser-api`：`ApiService` 持有关闭的工具名单（启动时读 `settings.json` 的 `mcp_disabled_tools`，运行中可替换，停止类工具与空名忽略）；`/api/system/info` 的 `tools_count` 不计关闭的工具，`mcp` 新增 `endpoints[]`。
  - `laser-host`：`configured_mcp_endpoints` = 主接入点（`mcp_endpoint` 或 `MCP_ENDPOINT`）+ `mcp_extra_endpoints` 前 4 项（无效、重复跳过；自建模式下附加接入点暂停连接，因为主接入点此时指向自建接入点）；`spawn_mcp_bridge` 为每个启用的接入点各起一条桥并返回条数，`stop_mcp_bridge` 停全部；进程内与内置语音两个后端都按开关拒绝；`voice_tools_list(api)` 去掉关闭的工具；`mcp_overview` 汇总接入点状态与身份、每个工具的开关与统计（地址脱敏）；`validate_disabled_tools` 只接受工具清单里可关闭的工具。
  - `moss-voice`：`voice_save_settings` 新增 `add_extra_mcp_endpoint`（`ws://` / `wss://`、不含占位符、不能与主 / 官方备份 / 自建接入点或已有项重复、最多 4 个）、`remove_extra_mcp_endpoint`（按编号 `extra-N`）、`disabled_tools`（整体替换，先由后端校验并立即生效，写文件失败时恢复原名单）；附加接入点在官方与内置模式下改动立即重连，工具开关改动在任何模式下都重连（让接入点重新获取工具清单），名单没变不重连；`settings.json` 一次写入（`save_all`），列表为空时删除对应键。`voice_status` 新增 `mcp`；切到自建模式的确认框提示附加接入点暂停。
  - `moss-voice-server`：`HostTools` 每轮重新读取工具清单，语音中心关闭或打开工具后，内置语音下一轮对话即生效。
  - 前端：语音中心新增“接入点与工具”卡片（`components/VoiceEndpointsCard.jsx`）——各接入点的状态（已连接 / 连接中 / 重连中 / 已暂停、自动重连次数、上次断开原因、对方客户端）与身份（token 中的智能体 ID 等与有效期，过期标红），添加 / 删除附加接入点；工具开关与调用统计（停止与急停工具的开关不可点）。纯逻辑 `endpointLabel`、`endpointView`、`identityView`、`toolSwitchPayload`、`toolStatText`、`extraEndpointError` 在 `voiceState.js`。
- **默认、契约与升级**：默认不变——没有 `mcp_extra_endpoints` 时只连主接入点，没有 `mcp_disabled_tools` 时全部工具打开。`settings.json` 新增两个可选顶层键 `mcp_extra_endpoints`（字符串数组）与 `mcp_disabled_tools`（工具名数组），旧版程序忽略它们（回退旧版后附加接入点不连接、工具开关失效）。`/api/system/info` 的 `mcp.endpoints` 为新增字段；`laser_mcp::run_bridge` 增加首个参数 `link_id`，`HostApplication::spawn_mcp_bridge` 改为返回启动的桥数量（仅仓库内部调用，均已更新）。调用统计只在内存中，重启清零。关闭工具的拒绝不写审计表（新增审计动作会让旧版程序读不了审计记录），只计入统计与事件；安全分级的拒绝照旧写审计。
- **验证（2026-10-08，partial）**：新增自动测试——`laser-contracts` 2 项（多接入点状态与重新登记、统计分类）；`laser-mcp` 4 项（关闭的工具不列出、调用被拒且不触达后端、停止类工具照常、未知工具名不新增统计条目；本机假接入点上桥在心跳间隔内发 Ping、对方不再回应时 1 秒空闲上限触发重连且连接次数为 2；连接失败原因记录且不含 token；身份只取标识字段、过期判断、非 JWT 返回空）；`laser-host` 2 项（附加接入点的无效 / 重复 / 上限与自建模式暂停、总览脱敏；开关从 `settings.json` 读入、内置语音清单与调用都生效、校验）；`laser-api` 2 处断言（`endpoints[]`、`tools_count` 不计关闭的工具）；`moss-voice` 2 项（附加接入点校验、保存、删除与重连规则，自建模式不重连与切换说明；工具开关生效、写文件、重连、拒绝时不改）；`moss-voice-server` 1 项（工具清单每轮重读）；桌面端 1 项（两处上限一致）；前端 1 项。原有回环桥测试改为按本接入点断言（多条桥可能同时在线）。`cargo test --workspace` 全部通过；`npm test` 86/86；预览桩检查卡片布局、开关提交的名单、地址校验与提交后清空输入。E2E（重新构建 release `moss-host` 后，本地回环 GRBL 模拟器）32/32；`npm run build` 成功；`cargo tauri build` 成功（`target/release/moss-desktop.exe`、`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe`）；`cargo tauri android build --target aarch64 --apk` 成功（`apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk`）。未验收：真实小智接入点上的多接入点、心跳与重连（官方接入点对 Ping 的响应、断网恢复）、token 实际载荷字段。
- **关联**：`crates/laser-contracts/src/{lib.rs,risk.rs}`、`crates/laser-mcp/{Cargo.toml,src/lib.rs}`、`crates/laser-api/src/lib.rs`、`crates/laser-host/src/lib.rs`、`crates/moss-voice/src/{center.rs,settings.rs,lib.rs}`、`crates/moss-voice-server/src/tools.rs`、`apps/desktop/src-tauri/src/{voice.rs,voice_builtin.rs}`、`apps/moss-host/src/main.rs`、`frontend/src/{voiceState.js,styles.css,pages/VoicePage.jsx,components/VoiceEndpointsCard.jsx}`、`frontend/tests/voiceState.test.mjs`；内置语音技术书 §13.9；实施清单 NV8。

### 新增：内置语音可选豆包流式识别与合成——边说边识别、边合成边播放（NV7）

- **现象与影响**：内置语音的云端识别与合成只有 OpenAI 兼容的整段接口：用户说完后才上传整段录音，合成也要等整句返回才开始播放，首句延迟里有一大段是在等识别与合成。
- **根因**：提供方接口只有“整段识别”“整句合成”两种同步调用，会话也只在说完后才把音频交给识别；没有可以边传边识别、边合成边返回的厂商实现。
- **修复**：
  - `moss-voice-providers`：新接口 `StreamingAsr` / `AsrStream`（开始说话即建立连接，`feed` 不阻塞，`finish` 等最终结果，丢弃即中止），`Tts::synthesize_stream`（默认整句一次送出，流式厂商覆盖）；线性重采样移到本 crate（`resample`，会话层重新导出）。
  - 新模块 `doubao`：帧协议移植自 univoice（`021113f` / 0.1.6，MIT，文件头注明来源）——识别 `sauc`（gzip 参数帧与 100 ms 音频包，最后一包序号取负）、合成 `bidirection`（连接 / 会话 / 任务事件）。连接、鉴权与错误处理按 MOSS 规则重写：tokio-tungstenite 0.29（与 axum 共用）+ rustls（明确指定 ring 与内置根证书）；新版控制台 `X-Api-Key`，旧版“APP ID:Access Token”自动改用 `X-Api-App-Key` / `X-Api-Access-Key`；“模型”一栏为资源 ID；错误只含中文原因，服务端说明经过滤（含地址或疑似密钥不回显）。相对 univoice 的修正：保留未知事件号（服务端句子事件 350–352 原实现会丢成“无事件”并误读会话号），`SessionFailed` 当作失败而不是一直等，解析越界报错不 panic。合成每句一个会话，空闲连接留给下一句复用，复用的连接已被服务端关闭时换新连接重试一次（已送出音频后不重试，避免重复播放）。
  - `moss-voice-server`：手动模式 `listen start` 时、自动模式在 VAD 确认开始说话时（环境静音不建连接）开始流式识别，补送说话前保留的 300 ms，之后每帧转 16 kHz 随收随送；说完把进行中的识别交给对话任务，失败时用整段音频再识别一次并记事件；重新拾音或打断时丢弃。合成的第一段带句子文字（`sentence_start`），后续分段 `SpeechMore` 用同一个重采样器接续。
  - `moss-voice`：`ProviderKind`（`openai` / `doubao`）；`resolve_builtin`（豆包只用于识别与合成，地址必须 `wss://`、留空用默认地址，资源 ID 留空用默认，合成音色必填、语速 0.5–2.0）；外部服务端链路的 `resolve` 拒绝豆包；接口类型参与“测试通过才能保存”的指纹。
  - 桌面端：豆包识别同时作为流式与整段识别；豆包合成时播放启动缓冲从 600 ms 降到 300 ms；“测试连接”对豆包走真实 WebSocket（识别送 1 秒低音量正弦音，合成一句短话）。
  - 前端：内置服务“模型配置”的识别、合成两栏新增“接口类型”（OpenAI 兼容 / 豆包流式），切到豆包自动填默认地址与资源 ID，标签随之改为 API Key、资源 ID、音色 ID。
- **默认、契约与升级**：默认仍为 OpenAI 兼容，不改变已有配置。`settings.json` 的 `voice.builtin_models.<分区>` 新增可选键 `kind`（只在豆包时写入，旧版程序可读）；`voice_builtin_models_test` / `voice_builtin_models_save` 的分区输入新增可选 `kind`；`voice_builtin_status` 的 `models.<分区>` 新增 `kind`。密钥仍存 `app_secrets` 的 `voice.asr.main` / `voice.tts.main`。Android 不涉及。
- **验证（2026-10-08，partial）**：新增自动测试——`moss-voice-providers` 13 项（协议字节与 univoice 测试一致、解析截断与越界、鉴权头新旧两种、语速换算、半个样本跨包拼接；本机假 WebSocket 服务上的流式识别边说边发包 / 序号连续 / 最后一包取负、整段识别 24 kHz 重采样、错误帧与 401 不回显密钥和地址、丢弃即断开、合成分段与连接复用、复用连接被关后重连一次、会话失败与错误帧）；`moss-voice-server` 4 项（说话期间识别已收到全部音频、自动模式静音不建连接且补送开头、流式失败退回整段、流式合成在整句完成前开始播放且 0 帧补静音，连续 6 次通过）；`moss-voice` 1 项、桌面端 1 项（默认值两处一致）、前端 1 项。真实服务（`#[ignore]`，本机运行通过）：用假密钥连接豆包真实地址，TLS 握手成功、服务端返回 401 `Invalid X-Api-Key`，错误不含密钥。`cargo test --workspace` 全部通过；`npm test` 85/85。E2E（本地回环 GRBL 模拟器）32/32；`npm run build` 成功；`cargo tauri build` 成功（`target/release/moss-desktop.exe` 与 NSIS 安装包 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe`）；`cargo tauri android build --target aarch64 --apk` 成功（`apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk`）；界面预览检查接口类型切换后地址与资源 ID 自动填入默认值、字段名随之变化。未验收：真实豆包密钥下的识别与合成、首句延迟对比（用户暂无豆包账号）、真实设备。
- **关联**：`crates/moss-voice-providers/{Cargo.toml,src/lib.rs,src/openai.rs,src/resample.rs,src/doubao.rs,src/doubao/{sauc.rs,bidirection.rs,tests.rs}}`、`crates/moss-voice-server/src/{conversation.rs,session.rs,audio/resample.rs,tests/chat.rs}`、`crates/moss-voice/src/{models.rs,settings.rs,builtin.rs,config_yaml.rs,lib.rs}`、`apps/desktop/src-tauri/src/voice_builtin.rs`、`frontend/src/{voiceState.js,components/VoiceBuiltinCard.jsx}`、`frontend/tests/voiceState.test.mjs`、`Cargo.lock`、`THIRD_PARTY_NOTICES.md`；文档 architecture §4c、api-contracts、frontend-behavior §9、quick-start“MOSS 内置语音服务”、内置语音技术书 n13 §13.8、实施清单 NV7。

### 新增：内置语音的本地模型——Silero 判停、SenseVoice 识别、Kokoro 合成（NV6）

- **现象与影响**：内置语音的识别与合成只能用云端接口，需要密钥、断网不可用；自动拾音只有能量判停，嘈杂环境下判停不准。
- **根因**：本地模型依赖的 sherpa-onnx 静态库含 GPL-3.0 的 espeak-ng，不能链接进 MIT 的 MOSS（技术书 §11.2，V5），此前没有独立的辅助程序与模型分发方式。
- **修复**：
  - 新 crate `crates/moss-voice-local-proto`（MIT）：MOSS 与辅助程序之间的帧协议。
  - 新程序 `apps/moss-voice-local`（**GPL-3.0-or-later**，独立工作区，附 `COPYING` 与 README）：判停、识别、合成各一个线程；构建 `cargo build --release --manifest-path apps/moss-voice-local/Cargo.toml --target-dir target`。
  - `moss-voice-providers::local`：启动与握手、按 id 匹配响应、超时、辅助程序退出时立即失败；`LocalAsr` / `LocalTts`。
  - `moss-voice-server`：`VadFactory` / `LocalVad`，出错时本会话退回能量判停并记事件。
  - `moss-voice::local_models`：三个模型的下载、SHA-256 校验（GitHub 发布页摘要）、安全解压（拒绝路径穿越与链接）、取消、删除，镜像可改。
  - 设置 `voice.builtin_local`（开关、音色、语速、镜像）；分区可用 = 本地已安装且打开或云端已配置，本地优先。
  - 桌面端：启动内置服务时按开关启动辅助程序并加载模型，停止时结束；四个新命令（已登记授权）；状态含 `local`。
  - 前端：内置服务卡片“本地模型”（模型表、进度、下载 / 取消 / 删除、开关、音色与语速、下载地址）。
- **默认、跨端与升级**：默认全部关闭，不下载任何模型；模型不进安装包，下载到运行目录 `models/`（卸载默认保留数据目录）。辅助程序需与 `moss-desktop.exe` 同目录——**安装包尚未包含它**（NV18 加入），开发时需单独构建。`settings.json` 新增可选键 `voice.builtin_local`。Android 不涉及。
- **许可**：MOSS 本体仍为 MIT；辅助程序为 GPL-3.0-or-later，分发时须附源码与 `COPYING`（`THIRD_PARTY_NOTICES.md` 已说明）。以上为一般理解，不构成法律意见。
- **验证（2026-10-07，partial）**：新增自动测试——协议 3、客户端 6、`LocalVad` 2、模型管理 5、设置 1、前端 1。真实环境（`#[ignore]`，本机运行通过）：真实下载 Silero 并校验；辅助程序加载三模型 2.8 秒、合成 / 识别 / 判停正确；端到端（合成语音当作设备上行 → Silero 判停 → SenseVoice 识别原句 → 假大模型 → Kokoro 回复）首句延迟 1078 ms、补静音 0 帧。预览桩走查界面三种模型状态、校验提示、保存参数与 420 px 布局。本机 `cargo test --workspace` 全部通过；`npm test` 84/84、E2E 32/32；`npm run build`、`cargo tauri build`（`target/release/moss-desktop.exe`、`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe`）、Android APK、辅助程序 `target/release/moss-voice-local.exe`（20.2 MB）均成功。未验收：真实设备；安装包（不含辅助程序）。
- **关联**：`crates/moss-voice-local-proto/`（新）、`apps/moss-voice-local/`（新）、`crates/moss-voice-providers/src/{local.rs,local/tests.rs,lib.rs}`、`crates/moss-voice-server/src/{audio/vad.rs,audio/mod.rs,lib.rs,session.rs,tests/chat.rs}`、`crates/moss-voice/src/{local_models.rs,settings.rs,center.rs,lib.rs}`、`apps/desktop/src-tauri/{build.rs,capabilities/default.json,src/lib.rs,src/voice_builtin.rs,permissions/autogenerated/voice_local_*.toml}`、`frontend/src/{voiceApi.js,voiceState.js,components/VoiceBuiltinCard.jsx,styles.css}`、`frontend/tests/voiceState.test.mjs`、`THIRD_PARTY_NOTICES.md`、根 `Cargo.toml`、`Cargo.lock`；文档 architecture §4c、api-contracts、quick-start、内置语音技术书 n12 §13.7、实施清单 NV6 / NV18。

### 变更：内置语音发起的运动与出光改为跟随严格模式开关（用户决定，取代 NV4 的“始终需要桌面批准”）

- **现象与影响**：NV4 让内置语音无论 `MOSS_FUSION_STRICT` 如何，运动与出光都只生成桌面审批；用户认为过严，希望与官方小智链路一致。**安全门禁变化**：严格模式关闭时，内置语音在模型带 `confirmed=true` 后直接执行加工，没有桌面批准。
- **根因**：NV4 在执行层按调用方强制严格模式（`CallerContext::Voice` 绕过“严格模式关闭即放行”）。
- **修复 / 行为**：`laser-api` 去掉对 `Voice` 的特殊分支，R3 跟随全局开关；`laser_host::VoiceBackend::strict_mode` 返回全局开关；`laser-mcp` 新增 `HostBackend::always_deny_r4`（内置语音为真），命令执行、打开应用、快捷键、打开网站等桌面控制工具不论开关始终拒绝并审计；内置语音默认系统提示要求先复述参数、等用户口头确认后才带 `confirmed=true`，停止类立即执行；语音中心切换确认框、卡片与链路说明按开关显示。
- **默认与升级**：默认 `MOSS_FUSION_STRICT=false` → 内置语音的加工只需口头确认。需要桌面批准时在运行目录 `.env` 设 `MOSS_FUSION_STRICT=true`。官方接入点、局域网、桌面的行为不变。
- **验证（2026-10-07，partial）**：`laser-api` 95 项（新增严格模式关闭直接发送测试，原测试改为严格模式开启下验证审批）、`laser-host` 5、`laser-mcp` 13、`moss-voice` 39、`moss-voice-server` 36 全部通过；`cargo test --workspace` 全部通过；`npm test` 83/83、E2E 32/32；`npm run build`、`cargo tauri build`（`target/release/moss-desktop.exe`、`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe`）、Android APK 均于本机成功。未验收：真实设备与真实模型（模型是否遵守“先口头确认”需实测）。
- **关联**：`crates/laser-api/src/{approval.rs,lib.rs}`、`crates/laser-mcp/src/lib.rs`、`crates/laser-host/src/lib.rs`、`crates/moss-voice-server/src/conversation.rs`、`crates/moss-voice/src/center.rs`、`frontend/src/{voiceState.js,pages/VoicePage.jsx}`；文档 safety、quick-start“MOSS 内置语音服务”、内置语音技术书 n11 §5.2 / §13.6。

### 新增：语音中心“MOSS 内置服务”链路与模型配置——内置语音可以对话并调用工具（NV5）

- **现象与影响**：NV3 / NV4 的对话引擎与工具调用已完成，但桌面端只能以回声模式启动内置服务：没有地方配置识别、大模型与合成，也没有把内置服务作为一种语音链路，用户无法真正用上内置语音。
- **根因**：语音中心只有官方 / 自建两种链路；内置服务没有模型设置与密钥存放，也没有在桌面端接上 MOSS 工具。
- **修复**：
  - `moss-voice`：`VoiceMode::Builtin`；`settings.json` 的 `voice.builtin_models`（地址、模型、音色、语速，不含密钥）；`builtin` 模块（保存计划：密钥留空沿用、删除分区同时删密钥；测试时留空用已保存密钥；状态视图只有 `configured` / `key_set`）；链路切换支持内置服务——切入时启动、失败整体回滚，离开时停止；接入点不变时不再重连小智桥。`HostHooks` 新增 `builtin_start` / `builtin_stop`。
  - 桌面端：模型密钥存 `app_secrets`（`voice.llm.main` / `voice.asr.main` / `voice.tts.main`）；三项都配置时以对话模式启动（OpenAI 兼容提供方 + 以 `CallerContext::Voice` 调用 MOSS 工具），否则回声模式；保存时若在运行则重启生效；语音链路为内置服务时随 MOSS 后台启动，失败原因显示在卡片上；新命令 `voice_builtin_models_test` / `voice_builtin_models_save`（已登记与授权）。
  - 设备会话数：`DeviceRegistry::sessions_changed` → `global_voice_telemetry`；`/api/system/info` 的 `voice.mode` 增加 `builtin` 并报告内置服务的会话数。
  - 修复（NV4 引入）：会话循环的 `select!` 随机选择就绪分支，对话任务同时送出“识别结果”与设备工具调用时，设备可能先收到 `tools/call` 再收到 `stt`（测试约 1/6 出现）。改为 `biased` 固定顺序：对话事件先于设备工具调用，设备消息先于播放定时器（打断不被积压的播放拍延迟）；修复后同一测试连续 15 次通过。
  - 前端：语音链路第三项“MOSS 内置服务”；内置服务卡片显示对话 / 回声测试模式、自动启动说明与失败原因，新增“模型配置”（先测试后保存、密钥不回显、删除需确认）；顶栏小智胶囊在内置链路下按设备会话数显示。
- **默认、契约与升级**：默认链路仍为官方，内置服务不会自动启用；`settings.json` 新增可选键 `voice.builtin_models`，旧文件照常读取；`/api/system/info` 的 `voice.mode` 新增取值 `builtin`（新增，非破坏）；schema 不变。内置语音的加工动作始终需要桌面审批（NV4）。Android 不编译语音服务与这些命令。
- **验证（2026-10-07，partial）**：`moss-voice` 39 项（新增 4 项）；`laser-api` 系统信息内置链路断言；桌面端 11 项（命令登记与授权一致、链路解析）；前端 `npm test` 83/83（新增 2 项）、`npm run build`；预览桩（演示数据）在内置浏览器走查三种状态、先测试后保存的调用参数与 420 px 宽布局。本机 `cargo test --workspace` 全部通过（`moss-voice-server` 36 项）；`npm run test:e2e` 32/32；`npm run build`、`cargo tauri build`（`target/release/moss-desktop.exe`、`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe`）、Android APK（`apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk`）均成功。未验收：真实模型（用户暂无）与真实设备。
- **关联**：`crates/moss-voice/src/{settings.rs,center.rs,builtin.rs,lib.rs}`、`crates/moss-voice-server/src/{lib.rs,session.rs}`、`crates/laser-api/src/lib.rs`、`apps/desktop/src-tauri/{Cargo.toml,build.rs,capabilities/default.json,src/lib.rs,src/voice.rs,src/voice_builtin.rs,permissions/autogenerated/voice_builtin_models_*.toml}`、`frontend/src/{voiceApi.js,voiceState.js,state/mcpStatus.js,components/VoiceBuiltinCard.jsx,pages/VoicePage.jsx,styles.css}`、`frontend/tests/{voiceState,mcpStatus}.test.mjs`；文档 quick-start“MOSS 内置语音服务”、frontend-behavior §9、api-contracts（系统信息、语音命令）、architecture §4c、内置语音技术书 n10 §13.5、实施清单 NV5。

### 新增：内置语音服务调用 MOSS 工具与桌面审批、设备自带工具中继（NV4，桌面端启用随 NV5）

- **现象与影响**：NV3 的对话引擎不能调用任何工具，用户无法用内置语音查询或控制激光机；同时需要保证语音永远不能绕过桌面审批直接让设备运动或出光。
- **根因**：调用方上下文里没有“内置语音”身份，审批来源、审计与工具分级门禁都只认小智接入点 / 局域网 / 网关；对话层没有接 MOSS 工具与设备自带 MCP 的通道。
- **修复**：
  - `laser-contracts`：`CallerContext::Voice { device_id, session_id }`（标签 `voice`，审计调用方 `voice:<设备>`）；`ApprovalSource::Voice`（`voice`）。
  - `laser-api`：`require_r3_authority` 与发送入口对 `Voice` **始终按严格模式处理**（全局 `MOSS_FUSION_STRICT` 关闭也只生成审批）；语音审批的 `session_id` 记发起设备。其他调用方行为不变。
  - `laser-mcp`：`HostBackend::audit_client()`（默认 `mcp`），分级门禁审计按后端身份记录。
  - `laser-host`：`VoiceBackend`（身份 `Voice`、严格模式恒为真）、`voice_tools_list()`（同严格模式接入点清单）、`voice_tool_call()`；进程内请求构造抽成 `in_process_request` 与 `InProcessBackend` 共用。
  - `moss-voice-server`：`tools::{ToolHost, HostTools}`——MCP 清单转模型工具定义、阻塞线程执行；结果为 `approval_required` 时直接朗读后端话术，不交模型改写；其他结果截断后交模型。设备自带 MCP：`initialize` → `notifications/initialized` → 分页 `tools/list`，工具以 `device__<名称>` 提供（非法字符换 `_`，调用时换回原名），经会话 WebSocket 转发 `tools/call` 并回填结果。
  - 前端审批卡来源标签：`voice` 显示“小智语音（MOSS 内置） · 设备 …末 4 位”。
- **安全门禁**：语音的 R3（发送、续雕、标定发送、设定原点等运动或出光动作、走边框）一律只生成桌面审批；R4 与未分级工具拒绝并审计；停止类动作照常立即执行。设备自带工具按 R1 直接执行。
- **默认与跨端**：桌面端仍以回声模式启动，`ToolHost` 接线与对话模式随 NV5；`/api/*` 契约不变；schema 不变（`approvals.source` 新增取值 `voice`）。Android 不编译语音服务。
- **验证（2026-10-07，partial）**：`laser-api` 新增 2 项（严格模式关闭时语音发送与设定原点仍只生成审批、来源与审计正确、批准后以审批身份执行；停止类动作不受影响）；`laser-host` 新增 1 项（清单不含永不开放的工具、拒绝并审计 `voice:<设备>`、只读工具照常）；`laser-contracts` 扩充 1 项；`moss-voice-server` 新增 4 项（共 36 项）；前端 `npm test` 81/81（来源标签断言 1 条）。本机 `cargo test --workspace` 全部通过；`npm run test:e2e` 32/32（release `moss-host` + 回环 GRBL 模拟器）；`npm run build`、`cargo tauri build`（`target/release/moss-desktop.exe`、`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe`）、Android APK（`apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk`）均成功。未验收：真实设备与真实模型。
- **关联**：`crates/laser-contracts/src/{lib.rs,approval.rs}`、`crates/laser-api/src/{approval.rs,lib.rs}`、`crates/laser-mcp/src/lib.rs`、`crates/laser-host/src/lib.rs`、`crates/moss-voice-server/src/{tools.rs,conversation.rs,session.rs,lib.rs,tests.rs,tests/chat.rs}`、`frontend/src/approvalState.js`、`frontend/tests/approvalState.test.mjs`；文档 architecture §2 / §4c、safety、db-schema §approvals、内置语音技术书 n9 §5.2 / §13.4、实施清单 NV4。

### 新增：内置语音服务对话引擎——云端识别、大模型（流式 + 工具调用）与合成（NV3，桌面端启用随 NV5）

- **现象与影响**：NV2 之后内置语音服务只能做回声，无法听懂用户、调用大模型并用语音回答。
- **根因**：没有模型提供方客户端，也没有“识别 → 大模型 → 合成”的对话编排。
- **修复**：
  - 新 crate `crates/moss-voice-providers`：`Asr` / `Llm` / `Tts` 接口与 OpenAI 兼容实现（识别 multipart 上传 WAV；大模型 `stream: true` + SSE 解码 + 工具调用按 index 拼接，兼容服务忽略流式；合成请求 WAV 并解析）；切句（首句遇逗号即切、之后句末切、180 字 / 6 秒强制送出、英文句点后跟空白才切）、Markdown 与表情清洗、表情映射；对话历史按长度预算整轮截断（工具调用与结果不拆开）；WAV 编解码。错误只含中文原因，不含地址与密钥；不跟随重定向；连接 10 秒、识别 / 合成 30 秒、大模型首包与断流 30 秒、总计 120 秒超时。
  - `moss-voice-server`：`ServerMode::Chat` 与 `conversation` 模块——每轮一个任务，边生成边逐句合成；工具并行执行、各 30 秒超时、最多 5 轮，工具可要求直接朗读；识别 / 大模型出错时说出错话术、单句合成失败跳过；退出词与空闲 120 秒后说再见再断开；唤醒后播放缓存的问候且不被紧随的 `listen start` 打断；打断时中止任务，已识别原话与已播句子记入历史；事件记录“识别完成：N 字”“首句延迟：X ms”，不记识别原文。
  - 节奏泵支持句子文字标记：播到该句第一帧时才发 `sentence_start`，设备屏幕文字与声音同步（回声也改用此方式）。
- **默认与跨端**：桌面端仍以回声模式启动，对话模式需要 NV5 的模型配置界面与密钥保存；NV3 不提供任何工具（MOSS 工具与审批在 NV4 接入）。Android 不编译。
- **验证（2026-10-07，partial）**：`moss-voice-providers` 20 项（含本机假 HTTP 服务 5 项，检查请求格式、流式拆包、错误提示不含密钥与地址）；`moss-voice-server` 32 项（新增对话端到端 9 项、启动校验 1 项、句子标记 1 项），连续运行 3 次稳定；`cargo test --workspace` 全部通过（首轮 `moss-voice` 既有测试 `start_stop_and_single_instance` 因测试取空闲端口的竞争失败一次，与本改动无关，单独重跑 5 次与整体重跑均通过，已另立修复任务）；`npm test` 81/81、`npm run build`、`cargo tauri build`（`target/release/moss-desktop.exe`、`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe`）、Android APK（`apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk`）均于本机成功；E2E 未跑（未改 HTTP 行为、前端与发送链路）。未验收：真实云端模型（用户暂无）与真实设备。
- **关联**：`crates/moss-voice-providers/`（新）、`crates/moss-voice-server/{Cargo.toml,src/lib.rs,src/session.rs,src/conversation.rs,src/audio/pump.rs,src/tests.rs,src/tests/chat.rs}`、根 `Cargo.toml`、`Cargo.lock`；文档 architecture §4c、内置语音技术书 n8 §13.3、实施清单 NV3。

### 新增：内置语音服务音频链路——Opus 编解码、静音判停、播放节奏泵与打断（NV2）

- **现象与影响**：NV1 的回声只是把设备上传的 Opus 字节原样发回：服务端不解码、不知道用户什么时候说完（自动模式固定录 3 秒），下行沿用 16 kHz，后续的识别与合成没有可用的音频链路。
- **根因**：`moss-voice-server` 还没有音频处理：无 Opus 编解码、无语音活动检测、无下行编码与播放节奏控制。
- **修复**：
  - 新模块 `crates/moss-voice-server/src/audio/`：`codec`（opus2 纯 Rust 后端 mousiki，不需要 CMake；编码 `Audio` 模式、复杂度 10，整个会话一个编码器）、`resample`（有状态线性插值）、`vad`（`Vad` 接口 + 双阈值能量 VAD，静音 200 ms 判停，背景噪声自适应）、`pump`（`PlaybackPump`：启动缓冲、供不上时补静音帧、尾帧补零、打断立即丢弃）。
  - 会话改为 空闲 → 拾音 → 播放 状态机：上行按设备 hello 的采样率解码；自动模式由 VAD 判停并保留语音前 300 ms；手动模式以 `listen stop` 为准；一段话最长 30 秒；下行声明 24 kHz，开头连发 3 帧后按绝对时钟每 60 ms 一帧；设备 `abort` 或新的 `listen start` 立即停播。
  - 回声模式改为“解码 → 判停 → 重采样到 24 kHz → 重新编码”播回，覆盖整条音频链路。
  - 空闲断开改为 120 秒没有说话也没有文本消息（自动模式下的静音帧不算活动）；无法解码的帧忽略、每会话只记录一次；每段播放记录“播放完成：N 帧，补静音 M 帧”。
  - 已授权设备先建好会话并登记，再回 hello，修复 NV2 初版中“设备已收到 hello、语音中心还看不到会话”的竞争（测试发现）。
  - `ServerConfig` 去掉 `echo_auto_window`，新增 `vad`、`downlink_sample_rate`（24000）、`playback_start_ms`（600）。
- **默认与跨端**：内置服务仍默认不启动；只影响内置语音服务的回声，不涉及工具、审批与加工。Android 不编译该服务。语音中心卡片说明文字随之更新。
- **计划调整**：Silero VAD 需要本地模型辅助程序（V5）与模型下载，随 NV6 接入同一 `Vad` 接口；NV2 用能量 VAD（嘈杂环境中可能判停不准，可改用按住说话）。
- **验证（2026-10-07，partial）**：本机 `cargo test --workspace` 全部通过；`moss-voice-server` 21 项（新增 14 项：编解码 2、重采样 3、VAD 4、节奏泵 3、端到端 2 项新增并改写 2 项——用真实 Opus 编码的音调与静音，覆盖手动 / 自动判停、帧协议 v1 / v2 / v3、坏帧、打断后 400 ms 内无音频帧、重新拾音），连续运行 5 次稳定。原 NV1 两项测试断言“回放字节与录入相同”，回声改为重新编码后不再成立，改为对解码音频断言帧数、音量与节奏（行为变更，非放宽）。`npm test` 81/81、`npm run build`、`cargo tauri build`（`target/release/moss-desktop.exe`、`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe`）、`cargo tauri android build --target aarch64 --apk`（`apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk`）均于本机成功。E2E 未跑：未改 HTTP 行为与发送链路。**未验收**：真实小智设备（清单 §2a HW-08）。
- **关联**：`crates/moss-voice-server/{Cargo.toml,src/lib.rs,src/session.rs,src/tests.rs,src/audio/}`、`Cargo.lock`、`frontend/src/components/VoiceBuiltinCard.jsx`；文档 architecture §4c、quick-start“内置语音服务（测试版）”、内置语音技术书 n7 §8 / §13.2、实施清单 NV2 / HW-08。

### 新增：内置语音服务第一步——小智设备直连 MOSS（回声测试版，NV1）

- **现象与影响**：此前小智设备只能连官方服务器或外部 xiaozhi-server，MOSS 无法直接与设备通信（内置语音技术书 §0）。
- **根因**：MOSS 没有小智设备协议的服务端实现。
- **修复**：
  - 新 crate `crates/moss-xiaozhi-proto`：设备消息（hello / listen / abort / mcp / iot / ping / goodbye，不认识的类型不报错）、按连接声明解析的音频帧协议 v1 / v2 / v3、与 xiaozhi-esp32-server 相同格式的设备 token（HMAC-SHA256，30 天）、OTA 请求解析与响应（不含 mqtt 与激活码）、版本比较。协议以 xiaozhi-esp32 固件 `0d576d3` 为准。
  - 新 crate `crates/moss-voice-server`：专用 tokio 运行时 + axum，OTA（默认 8003）与设备 WebSocket（默认 8000）两个端口，只提供 `/xiaozhi/*`；每台设备一个会话；未允许、已撤销或 token 无效的设备握手后收到设备屏幕提示（`alert`）并断开；回声模式把设备说的话按 60 ms 节奏原样播回，可打断；120 秒无消息断开；端口被占用时启动直接报错。
  - `laser-runtime` schema v6：设备登记表 `voice_devices`（待允许 / 已允许 / 已撤销；同一 MAC 换了 Client-Id 时退回待允许）；设备 token 密钥存 `app_secrets`（`voice.device_token_secret`，首次启动时随机生成，接口不返回）。
  - 桌面端 `apps/desktop/src-tauri/src/voice_builtin.rs`：`voice_builtin_status` / `start` / `stop`、`voice_device_set_status`、`voice_device_rename` 五个 Tauri 命令（已登记并授权）；退出时在急停之后停止内置服务。
  - 语音中心新增“内置语音服务”卡片（启动 / 停止、设备 OTA 地址、设备允许与撤销、改名、最近事件）与两个端口设置；`moss-voice` 设置新增 `builtin_ws_port` / `builtin_http_port`（默认 8000 / 8003，不能相同）。
- **默认与跨端**：默认不启动，只有在语音中心点“启动”才监听端口；现有官方链路、外部服务端链路与加工、急停不受影响。Android 不编译该服务（命令直接返回错误）。回声模式不调用任何工具。
- **验证（2026-10-07，partial）**：`npm test` 81/81（新增 1 项，用户电脑工作区）。Rust 新增测试：协议 13 项、服务端 7 项（真实绑定本机端口，用 reqwest 走 OTA、用 tungstenite 扮演设备）、Host 头解析 1 项、设备登记 1 项；云端无法访问 crates.io，**均未编译运行**，待用户本机 `cargo test --workspace`（首次会下载 axum、hmac 等依赖）。未验收：真实小智设备。2026-10-07 本机复核：`cargo test --workspace`、`npm test` 81/81 通过。
- **关联**：`crates/moss-xiaozhi-proto/`（新）、`crates/moss-voice-server/`（新）、根 `Cargo.toml`、`crates/laser-runtime/src/{db.rs,lib.rs}`、`crates/moss-voice/src/{settings.rs,center.rs}`、`apps/desktop/src-tauri/{Cargo.toml,build.rs,capabilities/default.json,src/lib.rs,src/voice_builtin.rs}`、`frontend/src/{components/VoiceBuiltinCard.jsx,pages/VoicePage.jsx,voiceApi.js,voiceState.js}`、`frontend/tests/voiceState.test.mjs`；文档 db-schema §3.13、architecture §4c、api-contracts §5、frontend-behavior §9、quick-start“内置语音服务（测试版）”、内置语音技术书 §13.1。

### 新增：NV0 前置验证工具与第三方声明（内置语音服务）

- **背景**：内置语音服务（`docs/integration/moss-native-voice-spec.md`）开工前，需要确认依赖库的许可证、Windows 编译条件与本地模型延迟（NV0）。
- **内容**：
  - 新增 `THIRD_PARTY_NOTICES.md`：haimen、xiaozhi-esp32-server、xiaozhi-client、univoice（Rust 版）四个 MIT 项目的提交号、用途与许可正文（已与各仓库 `LICENSE` 逐行核对）。
  - 新增 `tools/nv0-probe`：独立 Cargo 工作区（不加入 MOSS 的 workspace，不改 `Cargo.lock`），试编 opus2（bundled）与 sherpa-onnx 1.13.8（static），测 Opus 16k / 24k 编解码、Silero VAD、SenseVoice 识别、Kokoro 合成（含首段延迟）、说话人特征的耗时与实时率，并报告可执行文件体积；`run.ps1` 检查 CMake、下载模型到系统临时目录、生成报告。
  - 技术书升 n4，新增 §11：依赖库核实结论；sherpa-onnx 预编译静态库含 GPL-3.0 的 espeak-ng，本地模型放独立辅助程序 `moss-voice-local.exe`（GPL-3.0），MOSS 保持 MIT（V5，用户已确认）。
  - 第一次本机试编：电脑未装 CMake，libopus（bundled）构建失败。探针改为默认用 opus2 的纯 Rust 后端 `backend-mousiki`（与 libopus 后端同一套 API），并检查解码音量比；装了 CMake 时 `run.ps1` 额外编译 C 版对比。
  - 实施清单：修正重复出现的 LINK 与旧版 NV 章节。
- **默认与跨端**：不改动 MOSS 产品代码与构建；探针只在用户手动运行时编译。
- **验证（2026-10-07）**：用户本机（i7-13650HX，未装 CMake）运行 `tools/nv0-probe/run.ps1` 全部通过：编译 82 秒，探针 19.8 MB；Opus 16k / 24k 往返正常；Silero VAD 实时率 0.003；SenseVoice 中英粤识别正确，实时率约 0.04；Kokoro 首段 0.40–1.21 秒，实时率约 0.55；说话人特征每段约 65 ms。详见技术书 §11.3。
- **关联**：`THIRD_PARTY_NOTICES.md`（新）、`tools/nv0-probe/`（新）、`docs/integration/moss-native-voice-spec.md`、`docs/integration/moss-fusion-implementation-todo.md`。

### 文档：内置语音服务方案（三项目融合，n3，未实施）

- **背景**：用户决定把 xiaozhi-esp32-server、海门、xiaozhi-client 的功能做进 MOSS 本身，相同功能取最优实现，本地与云端模型都支持，不再依赖外部服务端。
- **内容**：新增 `docs/integration/moss-native-voice-spec.md`（n3）——三项目源码调研（提交 `7a260ee` / `513d9e6` / `fe3c382`，均 MIT）、功能对照与取舍、目标架构（桌面端内置 Rust 语音服务，设备 OTA 指向 MOSS；本地模型用 sherpa-onnx，无需 Python）、会话与工具调用规则（语音会话身份；R3、第三方 MCP、Claude Code / Codex 每次需桌面批准）、第三方 MCP / 记忆 / 视觉 / 插件（天气、新闻、农历、搜索、RAGFlow、音乐、Home Assistant）/ Agent CLI / 声纹 / 固件下发的设计（Home Assistant 控制默认需批准、门锁等始终需批准；声纹只存特征向量；固件默认不推送、逐型号确认）、安全规则（设备首次连接需允许、设备端口与 MOSS API 分离、密钥存 `app_secrets` 且接口不返回原文）、NV0..NV18 计划与已确认的决定 V1–V4。融合技术书升为 r11（取消“不重写上游源码”，说明 §9.4.4 与 A-67 只适用于外部服务端链路）；实施清单新增 NV 章节。
- **默认与跨端**：文档变更，无代码与行为变化。
- **验证**：文档-only；调研结论按源码文件与行号核对；`univoice` 源码、`rmcp` 传输支持、sherpa-onnx 实际体积与延迟留待 NV0。
- **关联**：`docs/integration/moss-native-voice-spec.md`（新）、`docs/integration/moss-desktop-fusion-technical-spec.md`、`docs/integration/moss-fusion-implementation-todo.md`。

### 新增：语音中心——链路切换、服务端启停、模型配置向导与页面（P4-03 / P4-05..P4-10）

- **现象与影响**：要改用自建小智服务端，用户需要手工改 MOSS 的 `mcp_endpoint`、自己记得开严格模式、在 MOSS 之外启动服务端、手工编辑服务端 YAML 填模型密钥；切换容易漏项（例如自建链路下模型可自行确认发送），密钥也容易被贴进 MOSS 设置或日志（技术书 §9.4、§9.5.1）。
- **根因**：上一步只完成了语音中心的后端基础（设置、进程管理、日志），没有编排、界面与退出联动。
- **修复**：
  - `crates/moss-voice` 新增 `center`（`VoiceCenter`，经 `HostHooks` 使用 MOSS 后端能力）：链路切换官方 / 自建互斥——切到自建先开严格模式，再一次写入 `voice` 与 `mcp_endpoint`（备份官方地址），再“先断旧桥再接新桥”；任一步失败恢复设置、严格模式与原来的桥（A-60、A-61）。切换前由 `mode_changes` 列出将改动的项。官方接入点也在这里编辑，官方模式下保存即重连。
  - `models`：大模型 / 语音识别 / 语音合成三栏的输入校验与最终地址（语音识别补全为 `/audio/transcriptions`，语音合成补全为 `/audio/speech`）；“测试连接”只用内存中的值发一次最小请求（大模型带一个工具定义并要求返回 `tool_calls`，语音识别发内置 1 秒音频，语音合成合成一句话），15 秒超时、不跟随重定向，错误信息不含地址与密钥。
  - `config_yaml`：按行读-改-写服务端 `data/.config.yaml`，只改技术书 §9.4.4 字段表中的键（`LLM.MossLLM`、`ASR.MossASR`、`TTS.MossTTS`、`selected_module`、顶层 `mcp_endpoint`），保留其他内容与注释；留空的分区不改，密钥留空保留原值；写入前备份 `.config.yaml.bak`，写入后用 `serde_yaml` 重新解析并逐项核对；原文件无法解析、使用单行写法或检测到 `manager-api.url`（全模块部署）时拒绝写入。保存要求每个分区与最近一次**测试成功**的内容完全一致（`model_test_required`）。
  - `health`：自建模式下每 5 秒请求接入点 `…/mcp_endpoint/health?key=…`，取 `robot_connections` 作为设备会话数写入顶栏遥测；未配置或失败显示“未知”，不猜。
  - `Supervisor::detach`：用户退出 MOSS 时选择保留服务端，则解除 Job Object 的“随 MOSS 关闭”。
  - 桌面壳 `apps/desktop/src-tauri/src/voice.rs`：10 个 Tauri 命令（`voice_status`、`voice_save_settings`、`voice_mode_changes`、`voice_set_mode`、`voice_server_start` / `stop` / `restart`、`voice_server_log`、`voice_models_test`、`voice_models_save`），在阻塞线程池执行；错误统一为 `{code, message, status}`。只有 MOSS 桌面窗口能调用；HTTP 没有 `/v1/voice/*`（404，A-68）；移动端命令直接返回 `voice_desktop_only`，`moss-voice` 不编入 Android（A-69）。
  - 退出联动（P4-03）：`exit_application` 新增 `stopServer`（默认是），顺序为先急停（`ShutdownCoordinator`）、再停止或放开服务端、最后退出；托盘“退出程序”在 MOSS 启动的服务端运行时弹对话框询问；关闭弹窗选择“完全退出”时显示“同时停止语音服务端”（默认勾选）。
  - `laser-host`：新增 `mcp_bridge_connected`、`set_voice_device_sessions`；桌面端把后端放入 `Arc` 与语音中心共享。
  - 前端：新页面 `/voice`（语音链路单选 + 确认框 + OTA 一致性提示、服务端 / 接入点 / 设备 / 严格模式四张状态卡、服务端与接入点设置、三栏模型配置“先测试后保存”、审批记录、脱敏日志），左侧第四个导航“语音中心”只在桌面端显示；`SettingsModal` 在桌面端把 MCP 地址输入换成“前往语音中心”（独立 `moss-host` 与手机仍保留输入框，`/api/settings` 兼容，A-77）。
- **默认与跨端**：默认仍为官方链路，未进入语音中心操作时行为不变。只有 Windows / macOS / Linux 桌面端有语音中心；Android、浏览器访问与独立 `moss-host` 没有入口与接口。模型密钥只写服务端配置文件，不进入 MOSS 的 SQLite、`settings.json` 与日志（A-67）。MOSS 不修改设备固件或 OTA 地址。P4-10 结论：首版不打包 Python 运行环境。
- **验证（2026-10-07）**：用户本机 `cargo test --workspace`、`npm test`、`npm run build`、E2E、`cargo tauri build`、Android 构建全部通过（另修复 `voice.rs` 一处未使用导入的警告）。此前云端：`npm test` 80/80 通过（新增 `tests/voiceState.test.mjs` 8 项，用户电脑工作区）；YAML 按行编辑逻辑用独立程序 + PyYAML 对 5 种文件形态（空文件、带注释与块标量、四空格缩进、`{}` 单行空映射、列表值）离线校验通过；预览桩 + Playwright 走查切换确认、先测试后保存、外部运行无停止、崩溃显示退出码、等待设备、全模块不可保存、窄屏无横向滚动、关闭弹窗传参，截图存 `frontend/tests/visual-baseline/voice-*.png`。**未覆盖**：Windows Job Object 解除与托盘对话框的人工验证；与真实 xiaozhi-server、接入点的联调；上游 `config_loader` 合并校验。
- **关联**：`crates/moss-voice/src/{center,models,config_yaml,health,supervisor,lib}.rs`、`crates/moss-voice/Cargo.toml`（新增 `reqwest`、`serde_yaml`）、`crates/laser-host/src/lib.rs`、`crates/laser-api/src/lib.rs`（测试）、`apps/desktop/src-tauri/{src/lib.rs,src/voice.rs,build.rs,Cargo.toml,capabilities/default.json}`、`frontend/src/{pages/VoicePage.jsx,voiceState.js,voiceApi.js,main.jsx,styles.css}`、`frontend/src/components/{SideNav,Navbar,SettingsModal,CloseConfirmModal}.jsx`、`frontend/tests/voiceState.test.mjs`；文档 quick-start §8、frontend-behavior §9、api-contracts §3.15 / §5、architecture §4b、feature-glossary；技术书 §9.4、§9.5.1，实施清单 P4-03、P4-05..P4-10，验收 A-60..A-69、A-77、A-78。

### 新增：语音中心后端基础——设置、服务端进程管理与日志（P4-01 / P4-02 / P4-04）

- **现象与影响**：自建小智服务端（xiaozhi-server）只能由用户在 MOSS 之外手动启动和查看日志，MOSS 不知道它是否在运行、是否崩溃，也无法在退出时一并处理（技术书 §9.4）。
- **根因**：语音中心尚未实现。
- **修复**：新增 crate `crates/moss-voice`（只供桌面端使用，不依赖 Tauri，不进入 `laser-api`、`moss-host` 与 Android）：
  - `settings`：运行目录 `settings.json` 的 `voice` 对象——链路模式（`official` / `self_hosted`，默认官方）、`server_dir`、`python`、`args`（默认 `app.py`）、`server_port`（默认 8000）、自建接入点地址、切换前备份的官方接入点地址、可选的接入点健康检查地址；读-改-写保留其他键，临时文件 + 重命名写入；旧版文件没有这些字段时按默认值读取。**模型密钥不保存在这里。**
  - `supervisor`：同一时刻最多一个由 MOSS 启动的服务端；启动前探测端口，被其他程序占用时记为“外部运行”，不再启动、也不能停止；停止先请求正常退出，5 秒后强制结束；Windows 用 Job Object 把服务端及其子进程绑在一起，MOSS 异常退出时也不会留下孤儿进程；子进程意外退出时记录退出码，**不自动重启**；启动前检查目录、Python 路径与 `data/.config.yaml`，缺失时给出明确原因。
  - `redact`：日志脱敏，`api_key`、`Authorization`、`token`、`password`、`secret` 等字段的值替换为 `***`，字段名保留；`tokens_used` 这类普通字段不受影响。
  - `logfile`：服务端输出逐行脱敏后写入运行目录 `logs/xiaozhi-server.log`，超过 5 MB 轮转，共保留 3 份；读取日志尾部时再脱敏一次。
- **默认与跨端**：本步只有后端基础，还没有接入桌面壳与界面（P4-05 / P4-07 / P4-08），对现有功能没有任何影响。Android 与 `moss-host` 不使用该 crate。
- **验证（2026-10-06，partial）**：新增 10 个测试：设置读写与旧版兼容（2）、脱敏（2）、日志轮转与尾部（2）、进程管理（4：A-62 启动 / 重复启动拒绝 / 停止且日志脱敏，A-63 外部实例不启动不停止，A-65 崩溃记录退出码且不自动重启，启动参数缺失原因）。云端在 Linux 上用去掉 serde 的副本运行其中 7 项全部通过（`sh` 假子进程）；Windows 分支（`cmd` 假子进程、`taskkill`、Job Object）只能在用户本机验证。**未执行**：`cargo test --workspace`（需用户本机）。
- **关联**：`crates/moss-voice/`（新）、根 `Cargo.toml`；技术书 §9.4.2、§9.4.3，实施清单 P4-01、P4-02、P4-04，验收 A-62、A-63、A-65、A-66。

### 新增：经审批执行的作业结果写回与“发送中崩溃”处理（P1-10）

- **现象与影响**：批准后作业开始发送时，审批结果只记“已开始执行”，作业最终完成、失败或被取消都不会写回；如果 MOSS 在发送过程中崩溃，重启后审批仍显示“已执行、成功”，与实际不符（技术书 §8，验收 A-33）。
- **根因**：审批服务在作业启动后就不再跟踪；启动恢复只处理 workflow 与 job 表，不处理审批。
- **修复**：
  - `ApprovalOutcome` 新增进展字段 `phase`（`running` / `completed` / `failed` / `cancelled` / `unknown`，旧记录没有该字段时为空），并派生 `Default`。
  - 经审批执行的发送在启动前记 `phase=running`（`ApprovalService::mark_running`）；`launch_send_job_unchecked` 新增可选的作业结束回调 `JobTerminalHook`，发送线程写回终态前调用 `record_job_result`：完成记“加工完成（已确认行数）”，失败或取消转为 `failed` 并记原因。只更新仍为 `running` 的审批，不覆盖其他结论；作业没能启动时照旧记失败。
  - 启动恢复（`recover_after_restart`）在过期待批准请求之外，把仍为 `running` 的已执行审批记为失败、`error_code=result_unknown`、`phase=unknown`，并写审计；与原有 workflow 恢复（发送中 → failed）一起保证“不重放”。
  - 设备动作与走边框等同步执行的审批，结果直接记 `phase=completed`。
- **默认与跨端**：只影响经桌面审批执行的作业；严格模式关闭时没有审批，现有发送行为不变。Windows / Android 一致。
- **验证（2026-10-06，partial）**：用户本机首次运行时 `restart_during_approved_send_marks_result_unknown_and_never_replays` 失败（重启后状态查询仍为“发送中”，即上一条的恢复缺陷；测试原用“只确认 2 行”的模拟设备，在途时序也不稳定），已修复恢复逻辑并改用“首条加工行阻塞到测试放行”的模拟设备；`laser-runtime` 新增 `restart_recovery_fails_sending_workflows_and_calibrations_in_payload_too`。`laser-api` 新增 5 个测试：A-31（无人处理的请求到期即过期、不能再批准）、A-33 单元（重启后结果未知、授权不能再用、迟到回写不覆盖、再次重启不重复处理）、作业结果只写回一次且已结束的作业不会被误判、A-33 集成（同一 SQLite 文件模拟“发送中崩溃 → 新进程启动恢复”：workflow 为 failed、审批结果未知、重启后的服务不连接设备、授权执行被拒）、经审批作业完成后结果写回“加工完成”。云端用 rustfmt 检查语法。**未执行**：`cargo test --workspace`（需用户本机）。
- **同时修复（启动恢复）**：`RuntimeDb::recover_after_restart` 原来只改 workflow 的状态列，记录正文里的 `status` 仍是 `sending`，而状态查询返回的是正文，所以重启后界面仍显示“发送中”；标定矩阵则完全不处理，重启后一直停在“发送中”。现在 workflow 与标定矩阵的列和正文同时改为 `failed` 并写入 `detail`（“host 重启中断，任务安全终止”），与 job 一样不会自动重发，需要重新生成预览后再发送。返回值仍为处理的 job 数量。
- **关联**：`crates/laser-contracts/src/approval.rs`、`crates/laser-contracts/src/lib.rs`、`crates/laser-api/src/approval.rs`、`crates/laser-api/src/lib.rs`、`crates/laser-runtime/src/db.rs`、`crates/laser-runtime/src/lib.rs`；`docs/developer/safety.md` §4、`docs/developer/architecture.md` §6；技术书 §5.5、§8，实施清单 P1-10，验收 A-31、A-33。

### 新增：全局审批卡与顶栏“审批”按钮（P1-09）

- **现象与影响**：严格模式下小智或局域网设备发起的加工只生成审批（P1-04 / P1-07），但桌面端没有界面查看和批准，只能等它过期。
- **根因**：审批接口已就绪，前端尚未实现技术书 §9.5.2 的审批卡。
- **修复**：
  - 新增 `frontend/src/components/ApprovalCenter.jsx`，挂在 `App` 根组件，任何页面都会弹出；每 4 秒读取待审批列表，有新请求时自动弹出。卡片显示刀路预览、内容、材料与厚度、功率（带标尺）/ 速度 / 遍数、尺寸与预计时长、目标设备（不显示 IP）、来源、摘要前后缀与 120 秒倒计时（按服务端时间）。
  - 必须勾选“我在机器旁，材料已放好、原点已确认，排风已开启”才能批准；`approval_mismatch` / `approval_expired` 等失败在卡内显示原因并只保留“关闭”，不发送任何指令；倒计时归零自动关闭；“拒绝”立即生效；“稍后处理”只关闭卡片。
  - 批准后加工任务与标定矩阵进入现有 `SendOverlay`（`STAGE_MAP` 四步不变），全程显示“取消后续发送”与“急停 0x18”（注明不代替控制器硬件急停）；顶栏急停在此期间同样有效。`SendOverlay` 新增可选属性 `onEmergencyStop`、`cancelLabel`、`badge`，不传时与原来一致。
  - 顶栏新增“审批”按钮：无请求为灰色，有请求变红并显示数量，点击打开审批卡。
  - 纯逻辑放在 `frontend/src/approvalState.js`（倒计时、批准门槛、错误说明、目标与摘要显示、记录字段整理），记录中缺失的字段显示“—”，不编造。
- **默认与跨端**：只有桌面端（有桌面会话）显示审批按钮与审批卡；浏览器、Android 与手机远程模式不轮询、不显示。严格模式关闭时不会产生审批，界面上只多一个灰色“审批”按钮。工作台、画板、材料实验室原有发送流程不变。
- **验证（2026-10-06，partial）**：新增 `tests/approvalState.test.mjs` 6 项（A-71 批准门槛、服务端时钟倒计时、错误说明、目标不显示地址、排序与新请求识别、记录字段整理），`npm test` 通过（用户电脑工作区）。云端用预览桩数据 + Playwright 验证：自动弹出、未勾选不可批准、稍后处理后从顶栏重新打开、摘要不一致在卡内显示且只剩“关闭”、批准后四步覆盖层带急停并走到完成、倒计时归零自动关闭并切到下一条、拒绝、窄屏布局，无页面报错；`frontend/tests/visual-baseline/` 新增 3 张截图。**未执行**：`npm run build`（用户电脑工作区缺少 rollup 原生包）、`cargo tauri build`、真机人工检查；E2E 未加入（现有 E2E 依赖真实后端，无法在测试中开启严格模式与桌面会话）。
- **关联**：`frontend/src/components/ApprovalCenter.jsx`、`frontend/src/approvalState.js`、`frontend/src/components/Navbar.jsx`、`frontend/src/components/SendOverlay.jsx`、`frontend/src/main.jsx`、`frontend/src/styles.css`、`frontend/tests/approvalState.test.mjs`、`frontend/tests/visual-baseline/`；`docs/developer/frontend-behavior.md` §8；技术书 §9.5.2，实施清单 P1-09，验收 A-70、A-71、A-74。

### 新增：安全审计记录写入 `audit_log`（P1-08）

- **现象与影响**：严格模式与桌面审批上线后，谁在什么时候发起、批准、拒绝了哪个运动或出光动作，哪些请求被拦下，都没有留下记录；`audit_log` 表从建库起一直是空的。
- **根因**：此前只有表结构与类型（P1-01 / P1-02），没有任何写入方。
- **修复**：
  - 审批服务（`crates/laser-api/src/approval.rs`）在每个状态变化时写审计：创建（`client` 为发起方：`mcp` / 配对 ID / `gateway`）、批准与执行（`desktop`）、拒绝（`desktop`，含原因）、过期与执行失败（`system`）。`detail` 只放审批 ID、类别、摘要、错误码与原因，不含设备地址、凭据或文件路径。
  - **审计写不进去时**（技术书 §7.3，A-34）：创建审批、桌面批准、执行前消费授权这三步与审计在同一数据库事务中，审计失败即整体回滚——不创建审批、状态不变、不连接设备，返回 503 `approval_unavailable`（说明审计写入失败）。拒绝、过期、执行失败与急停等降低风险的动作不依赖审计，照常生效。
  - 小智带 `confirmed=true` 发起的 R3 请求：额外记一条 `confirmed_field_stripped`（`removed_fields: ["confirmed"]`），并从审批保存的执行参数中删除该字段；批准后执行时由桌面批准提供确认（A-20）。
  - 严格模式拦下的本机回环 R3 请求记 `strict_mode_blocked`（`client=local`，结果 `denied`）。
  - 小智接入点在严格模式下拒绝的 R4 工具与采用调参建议，通过 `HostBackend::audit`（新增带默认空实现的方法，已有实现无需修改）记 `r4_denied` / `strict_mode_blocked`；`laser-host` 的进程内后端写入本地数据库。
  - `RuntimeDb::approvals_due_for_expiry` 列出将过期的审批，供逐条记审计。
- **默认与跨端**：严格模式关闭且没有审批时不写任何审计，现有行为不变。Windows / Android 一致。
- **验证（2026-10-06，partial）**：`laser-api` 新增 4 个测试：审批全生命周期审计顺序与内容（不含地址）、A-34 单元（审计失败时创建 / 批准 / 执行被拒且状态不变，拒绝与过期照常）、A-20 集成（回环拦截 → 小智发起 → 删除确认 → 桌面批准 → 执行，审计顺序与 `client` 正确，保存的参数不含 `confirmed`）、A-34 集成（审计失败时回原点被拒且不连接设备、急停照常执行、回环拦截照常）。`laser-mcp` 严格模式拒绝测试补充审计断言。云端用 rustfmt 检查语法。**未执行**：`cargo test --workspace`（需用户本机）。
- **关联**：`crates/laser-api/src/approval.rs`、`crates/laser-api/src/lib.rs`、`crates/laser-runtime/src/db.rs`、`crates/laser-mcp/src/lib.rs`、`crates/laser-host/src/lib.rs`；`docs/developer/safety.md` §4、`docs/developer/db-schema.md` §3.11、§6；技术书 §7.3，实施清单 P1-08，验收 A-20、A-34。

### 新增：手机 App 连接电脑 MOSS（电脑为唯一数据源，手机只读材料与设置）

- **现象与影响**：手机 App 内置独立后端，材料库、任务记录与电脑互不相通；用户希望两端数据一致，并能用手机操作雕刻机。
- **根因**：此前只有“手机浏览器访问电脑”一条路，且需手动设置 `LASER_WEB_HOST`、在命令行取配对码；App 本身只会连接手机内置后端。双向同步方案（USN / CRDT / 冲突副本）对材料参数存在静默覆盖风险，参照 OctoPrint / Klipper 选择主机为唯一数据源（技术书 §9.8）。
- **修复**：
  - `laser-runtime`：`web_host_from`，`settings.json` `lan_access=true` 时监听 `0.0.0.0`（`LASER_WEB_HOST` 优先）。
  - `laser-api`：`is_reference_data_write` 与 `lan_read_only` 守卫——局域网调用方不能改材料库（除导出）、不能把校准单元格存为推荐参数（`select-cell`）、`/api/settings`、Jev 测试 / 查看密钥；`/api/settings` 接受 `lan_access`；新增仅回环的 `GET /api/system/lan-info`。
  - 前端：`remoteHost.js`；`authSession` 远程模式（请求发往电脑、不取不发桌面会话、配对 token 存 `localStorage`）；偏好设置新分区 `PhoneLinkSection`（电脑：允许连接 / 地址 / 生成配对码；手机：连接 / 断开）；远程模式隐藏电脑侧设置项；启动失败页“改用手机本机模式”；配对成功后重载；材料页只读：不显示套用最近 / 导入 / 新建 / 管理材料 / 保存到材料库，参数输入框只读、校准单元格不可点选保存，材料与校准结果照常显示、导出库保留；预览图远程模式改为鉴权 blob，工作台 G-code 文本改用 `apiFetch`（原 `fetch` 不带 token）。
  - Android：`network_security_config.xml` 允许明文 HTTP（电脑局域网地址不固定）。
- **默认与跨端**：默认不开启，电脑仍只监听本机；开启后局域网访问仍须配对。手机本机模式不变。雕刻机操作经电脑执行，严格模式开启时手机的发送走桌面审批。**行为变化**：局域网浏览器访问此前可改材料与设置，现在只读。
- **验证（2026-10-06，partial）**：前端 `npm test` 66 项通过（新增 `tests/remoteHost.test.mjs` 5 项，用户电脑工作区）；改动 JSX 经 bun 转译语法检查通过。新增 Rust 测试 `web_host_prefers_env_then_lan_access_then_loopback`、`reference_data_writes_are_classified`、`paired_lan_client_cannot_change_materials_or_settings_but_loopback_can`、`lan_info_is_loopback_only`，**未编译运行**（当前环境无 Rust 工具链）。未执行：`cargo test --workspace`、`npm run build`、E2E、Windows / Android 构建与真机 A-89..A-96。
- **关联**：`crates/laser-runtime/src/config.rs`、`crates/laser-api/src/lib.rs`、`frontend/src/{remoteHost.js,authSession.js,main.jsx}`、`frontend/src/components/{PhoneLinkSection,SettingsModal,AuthModal}.jsx`、`frontend/src/pages/{HomePage,DrawPage,MaterialLabPage}.jsx`、`apps/desktop/src-tauri/gen/android/app/src/main/res/xml/network_security_config.xml`、技术书 §9.8、quick-start §7。

### 新增：桌面会话与桌面审批接口（P1-07）

- **现象与影响**：严格模式（P1-04）开启后，小智或局域网发起的运动与出光动作只会生成审批，但桌面端没有办法查看和批准；桌面端自己的请求也被当作“本机其他程序”拒绝。
- **根因**：后端无法区分桌面窗口与本机任意进程（都是回环、免鉴权），也没有审批的查询与批准接口。
- **修复**：
  - **桌面会话**：桌面壳启动时由后端生成 32 字节随机会话值（`ApiService::enable_desktop_session`），新命令 `get_desktop_session` 交给本窗口页面，只存内存。请求头 `X-Moss-Desktop-Session` 只在回环连接上有效，常量时间比较；值不对、后端没有会话（独立 `moss-host`）、或来自局域网时 403 `desktop_session_invalid`。有效时调用方上下文为 `Desktop`。Android 上该命令返回“移动端没有桌面会话”。命令已登记到 `build.rs` 与默认 capability。
  - **审批接口（仅桌面会话，其他身份 403 `desktop_session_required`）**：`GET /v1/approvals?status=pending` 列表，附 `target{mode, label}`（网络设备只显示“网络设备”，不含地址）与 `server_time`、`ttl_seconds`；`POST /v1/approvals/{id}/approve` 提交审批卡上的 `digest`，后端按当前记录重算摘要，三者一致才批准，随后立即以审批身份重放原请求——使用审批保存的连接参数、`confirmed=true`，走全部现有门禁，并由执行入口单次消费授权；`POST /v1/approvals/{id}/reject` 拒绝。
  - **schema v5**：`approvals` 新增 `request` 列保存原始执行参数（去掉连接字段、身份字段与幂等键，只在本地库、接口不返回），供批准时重放与重算摘要。`ApprovalRecord` 新增同名字段，同样不参与序列化。
  - **前端**：`authSession.refreshDesktopSession()` 在渲染前取会话（取不到不影响启动）；`fetchWithDesktopSession` 只对本机相对路径加会话头，收到 403 `desktop_session_invalid` 时重新取会话并只重试一次；`apiFetch` 与画板页改用它。
- **默认与跨端**：严格模式关闭时，带有效会话的请求只是上下文从 `Loopback` 变为 `Desktop`，所有判定不变；浏览器与 Android 不发送会话头，行为不变。现有数据库自动升级到 v5（新增一列，旧程序仍可打开）。审批卡界面在 P1-09 实现，本步只有接口。**严格模式仍建议在 P1-09 完成后再实际开启**：现在开启后桌面端自己的发送已可正常执行，但还没有界面批准小智发来的请求。
- **验证补充（2026-10-06）**：用户本机 `cargo test --workspace`、`npm test`、`npm run build`、`cargo tauri build` 通过；在安装版 `%APPDATA%\moss\.env` 临时开启严格模式实测：桌面端发送通过严格模式检查，普通浏览器发送被拒，无会话访问 `/v1/approvals` 得到 `desktop_session_required`，测试后已关闭。
- **验证（2026-10-06，partial）**：`laser-api` 新增 5 个集成测试：会话头校验与 `Desktop` 上下文（未开启会话、错误值、非回环均拒绝，有效会话在严格模式下可直接发送）、A-75（审批接口只接受桌面会话、状态参数校验、不存在的审批 404）、A-21（小智发起 → 桌面列表不含地址 → 错误摘要 409 → 批准后按保存的目标执行并完成 → 重复批准 `approval_consumed`）、A-22（创建后刀路变化，批准得到 `approval_mismatch` 且不连接设备）、拒绝后不可批准与安全动作经桌面批准执行。`laser-runtime` 新增 v5 迁移测试；既有迁移测试中写死的版本号改为 `SCHEMA_VERSION`，模拟 v3 状态时删除条件改为 `version>=4`——schema 升级的必要更新，不是放宽断言。`laser-contracts` 记录序列化测试补充 `request` 不输出。前端新增会话测试，`npm test` 61 项通过（用户电脑工作区）。云端用 rustfmt 检查语法、用 SQLite 验证 v5 语句。**未执行**：`cargo test --workspace`、`npm run build`（用户电脑工作区缺少 rollup 的 Linux 原生包）、`cargo tauri build`、Android 构建。
- **关联**：`crates/laser-api/src/approval.rs`、`crates/laser-api/src/lib.rs`、`crates/laser-runtime/src/db.rs`、`crates/laser-runtime/src/lib.rs`、`crates/laser-contracts/src/approval.rs`；`apps/desktop/src-tauri/src/lib.rs`、`build.rs`、`capabilities/default.json`；`frontend/src/authSession.js`、`frontend/src/api/client.js`、`frontend/src/main.jsx`、`frontend/src/pages/DrawPage.jsx`、`frontend/tests/authSession.test.mjs`；`docs/developer/api-contracts.md` §5、`docs/developer/frontend-behavior.md` §1、`docs/developer/db-schema.md`；技术书 §5.3–§5.5、§9.5.4，实施清单 P1-07，验收 A-21、A-22、A-75。

### 新增：小智工具风险分级 R0–R4 与接入点严格模式处理（P1-06）

- **现象与影响**：严格模式（P1-04）只在 HTTP 执行入口拦截运动与出光；小智接入点仍会把打开应用、快捷键、桌面命令、删除材料库等调用直接执行，也没有一张“每个工具属于哪一级”的表可供审查（技术书 §6.1）。
- **根因**：工具分级只存在于技术书表格，代码中没有对应函数。
- **修复**：
  - `laser-contracts` 新增 `risk` 模块：`RiskLevel`（R0 只读、R1 生成、R2 降低风险、R3 运动或出光、R4 永不开放）与 `tool_risk(name, arguments)`，按工具名与 action、confirmed、send_after_generate、calibration_id、dry_run、feedback_id 判定；不认识的工具返回 `None`，按 R4 处理。材料库的保存、导入、删除都归入 R4（技术书“材料库删除 / 覆盖”，保存与导入可能覆盖已有参数）；`laser_grbl_tool` 的 `send_command` / `send_file` 归入 R4。`tool_never_exposed` 列出无论参数都属 R4 的四个桌面工具。
  - `laser-mcp`：`HostBackend` 新增带默认实现的 `strict_mode()`（默认 false，已有实现无需修改）；`laser-host` 的进程内后端返回 ApiService 的严格模式开关。严格模式下：R4 与未分级工具返回 `tool_not_allowed`，不执行、不触达后端；采用反馈建议（R3，但执行层没有审批检查点）返回 `strict_mode_blocked`，提示到桌面端操作；其余 R3 照常转发，由执行层生成审批并播报“请在电脑上确认”；R0–R2 不变。`tools/list` 在严格模式下不再列出四个永不开放的工具，`/api/system/info` 的 `mcp.tools_count` 同步减少。
  - 分级函数放在 `laser-contracts` 而不是 `laser-mcp`，以便以后可选网关（P2b，位于 laser-api）复用。
- **默认与跨端**：严格模式关闭时（默认）接入点行为完全不变。Windows / Android 一致。
- **验证补充（2026-10-06）**：用户本机 `cargo test --workspace` 全部通过。
- **验证（2026-10-06，partial）**：`laser-contracts` 新增分级全表测试（43 组工具与参数）；`laser-mcp` 新增 4 个测试：工具清单与工具目录中的每个工具都有分级（新增工具忘记分级即失败）、A-28（严格模式下 R4 与材料库删除 / 保存、任意 G-code、未知工具被拒且不触达后端，`tools/list` 去掉 4 个工具）、A-20 / A-25（`confirmed=true` 的 `confirm_send` 只得到 `approval_required`，急停照常转发）、A-40（严格模式关闭时行为不变）。云端用 rustfmt 检查语法。**未执行**：`cargo test --workspace`（需用户本机）。
- **关联**：`crates/laser-contracts/src/risk.rs`、`crates/laser-contracts/src/lib.rs`、`crates/laser-mcp/src/lib.rs`、`crates/laser-host/src/lib.rs`、`crates/laser-api/src/lib.rs`；`docs/developer/safety.md` §4；技术书 §6.1–§6.3，实施清单 P1-06，验收 A-20、A-25、A-28、A-40。

### 新增：严格模式——运动与出光动作需要桌面授权（P1-04，默认关闭）

- **现象与影响**：小智对话或局域网客户端可以带上 `confirmed=true` 直接触发发送、走边框、回原点等真实动作，桌面端无从拦截（技术书 §6.2）。
- **根因**：执行入口只检查 `confirmed=true` 与设备门禁，不区分请求来自谁；P1-00 只把调用方上下文传到了入口，P1-03 只有审批服务。
- **修复**：
  - 新增开关 `MOSS_FUSION_STRICT`（`laser_runtime::config::fusion_strict_from_env`）：默认 `false`；取值写错、或 `MOSS_GATEWAY_ENABLED=true` 而严格模式关闭时，宿主启动失败并给出原因，不静默当作关闭。
  - 唯一检查点 `require_r3_authority`（`crates/laser-api/src/approval.rs`），接入全部 R3 执行入口：`launch_send_job_for`（workflow `confirm_send` / `send` / 断点续雕、文字任务发送、标定矩阵发送）、`/api/safe-action` 的 R3 动作、`/api/device/framing`。规则：桌面会话放行；小智桥接 / 局域网配对 / 网关按显式目标（或 `.env` 默认值）创建审批，返回 HTTP 202 `{status: "approval_required", approval_id, expires_at, summary, speech}`，不连接设备、不改记录状态，同一 `request_id` 不重复创建；本机回环无凭据返回 403 `strict_mode_blocked`；以审批身份执行时按当前要素与审批保存的连接重算摘要并单次消费，执行**使用审批保存的连接参数**，结果写回审批（成功记作业 ID，失败记 failed 不可重用）；无本地数据库时 503 `approval_unavailable`。
  - 安全动作分级 `laser_contracts::safe_action_requires_approval`：只有查询状态、急停、软复位、关激光、倍率复位与降低属于不需授权的类别，其余（解锁、回原点、设原点、移动、提高倍率、未来新增动作）一律按 R3 处理。
  - 宿主启动时把遗留的待批准 / 已批准审批全部标为过期（A-32）。
  - `/api/device/framing` 的设备阶段原样抽成 `run_framing`，路由先做授权检查；HTTP 状态行补充 202 / 409 / 410 / 503 的原因短语。
- **默认与跨端**：严格模式默认关闭；关闭时发送入口直接走原逻辑、不读取任何额外数据，现有网页、桌面、小智行为不变。**桌面会话（P1-07）接入前不要在实际使用中开启**：开启后桌面端自己的发送也会被当作本机回环拒绝。急停、取消、软复位、降低倍率在任何模式下都不受限制。Windows / Android 一致。
- **验证补充（2026-10-06）**：用户本机 `cargo test --workspace` 全部通过。
- **验证（2026-10-06，partial）**：`laser-api` 新增 6 个集成测试（FakeGrbl 记录传输）：A-24 / A-26（严格模式下回环发送、走边框、回原点被拒且不连接设备）、A-27（停止类动作照常执行）、A-25 / A-21（小智桥接发送只生成审批、未批准不能执行、批准后使用保存的目标执行并完成）、A-23（安全动作授权单次使用、不能挪用到其他动作）、桌面会话放行与无数据库时失败即拒绝、A-40（严格模式关闭时行为不变）；`laser-contracts` 新增安全动作分级测试；`laser-runtime` 新增严格模式配置测试。云端用 rustfmt 检查语法。**未执行**：`cargo test --workspace`（需用户本机）。
- **关联**：`crates/laser-api/src/approval.rs`、`crates/laser-api/src/lib.rs`、`crates/laser-contracts/src/approval.rs`、`crates/laser-contracts/src/lib.rs`、`crates/laser-runtime/src/config.rs`、`crates/laser-host/src/lib.rs`；`docs/developer/safety.md` §3、§4；技术书 §5.1、§6.1–§6.4，实施清单 P1-04，验收 A-21、A-23..A-27、A-40。

### 修复：Android 启动即显示“MOSS 后端无法启动 / Read-only file system”

- **现象与影响**：签名后的 Android release APK 能安装，但打开后停在启动失败页：“后端服务初始化失败: 运行时目录初始化失败: Io(Os { code: 30, kind: ReadOnlyFilesystem })”，所有页面不可用。Windows 不受影响。
- **根因**：桌面壳在 `run()` 一开始调用 `HostApplication::init()` → `RuntimeRoot::resolve(None, None)` → `default_base()`。Android 进程没有 `APPDATA`，回退为“当前目录/.runtime”，而 Android 进程的当前目录是只读的 `/`，`create_dir_all("/.runtime")` 失败。此时 Tauri 尚未启动，拿不到应用私有数据目录。
- **修复**：`laser-host` 新增 `HostApplication::init_with_runtime_dir(Option<&Path>)`（`init()` 委托它，桌面与 `moss-host` 行为不变）；桌面壳抽出 `start_backend`，桌面端仍在 `run()` 开头以默认目录启动；移动端改在 Tauri `setup` 中用 `app.path().app_data_dir()`（应用私有目录）启动后端，再登记 `BackendOrigin` 与导出状态，后端句柄 `Box::leak` 保持与进程同寿命。
- **默认与跨端**：Windows 数据目录仍为 `%APPDATA%\moss`；Android 数据改存应用私有目录（卸载应用时由系统一并删除）。无设备、发送或门禁变化。同时为 Android release 构建接入签名：`gen/android/app/build.gradle.kts` 读取 `gen/android/keystore.properties`（已被忽略、不进仓库），文件不存在时保持未签名。
- **验证（2026-10-06，partial）**：用户真机截图确认原故障；本次改动**未编译**（当前环境无 Rust 工具链），待用户本机 `cargo test --workspace`、`cargo tauri build`、`cargo tauri android build --target aarch64 --apk` 并真机启动确认进入主页。
- **关联**：`apps/desktop/src-tauri/src/lib.rs`（`start_backend`、mobile `setup`）、`crates/laser-host/src/lib.rs`（`init_with_runtime_dir`）、`crates/laser-runtime/src/root.rs`（`default_base`）。

### 新增：桌面审批服务（P1-03）

- **现象与影响**：严格模式下，小智或局域网发起的发送、走边框、安全动作要先在桌面批准（技术书 §5.3–§5.5）。P1-01 / P1-02 只有类型和表，没有创建、批准、过期与“只能用一次”的逻辑。
- **根因**：融合方案新增能力，审批服务尚未实现。
- **修复**：新增 `crates/laser-api/src/approval.rs`：
  - `ApprovalService`：创建（带 `request_id` 时按 `(source, request_id)` 幂等）、读取与列表（顺带处理到期）、批准（审批卡提交的摘要与按当前状态重算的摘要都必须与创建时一致，否则 `approval_mismatch` 且不批准）、拒绝、到期、进程重启时全部 pending / approved 过期、执行前单次消费（已批准、未过期、摘要一致才改为 executed；执行时摘要不一致则授权作废为 failed）、记录执行结果（失败记 failed，不可重用）。有效期取 `MOSS_APPROVAL_TTL_SECONDS`（默认 120 秒，30..600）。
  - 摘要：`approval_digest` = `SHA-256(canonical_json(payload))`，payload 含动作要素、类别、目标记录与 `target{mode, host_fingerprint}`；`host_fingerprint` 是解析后连接描述的哈希，摘要里没有地址明文；请求里夹带的 `kind`、`workflow_id`、`target` 不能覆盖服务端的值。`record_digest_facts` 从记录取 G-code SHA、材料、厚度、功率 / 速度 / 遍数与发送方式，重新预览或改发送方式都会使旧授权失效。
  - 目标解析：`resolve_approval_target` 只用请求中的显式参数与 `.env` 默认值（`LASER_DEFAULT_CONNECTION_MODE`、`LASER_NETWORK_HOST`、`LASER_NETWORK_TELNET_PORT`、`GRBL_DEFAULT_PORT`、`GRBL_BAUDRATE`），**不枚举系统串口自动选择**；拿不到目标返回 `target_required`。（现有网页发送在串口留空时仍会选第一个串口，行为不变。）
  - `laser-runtime`：`insert_approval`、`get_approval`、`find_approval_by_request`、`list_approvals`、`transition_approval`（只在当前状态匹配时改动，单条 UPDATE 原子，并按 P1-01 的迁移规则拒绝非法迁移）、`consume_approval`、`set_approval_outcome`、`expire_approvals`。
  - 新增错误码：`target_required`（400）、`approval_not_found`（404）、`approval_expired`（410）、`approval_consumed` / `approval_rejected` / `approval_not_approved` / `approval_mismatch`（409）、`approval_invalid_request`（400）、`approval_unavailable`（503）。接口契约在接入路由（P1-07）时写入 api-contracts。
- **默认与跨端**：服务**尚未接入**任何路由或执行入口，现有发送、门禁与设备行为不变；严格模式接入在 P1-04，桌面审批接口在 P1-07。Windows / Android 一致。
- **验证补充（2026-10-06）**：用户本机 `cargo test --workspace` 全部通过。
- **验证（2026-10-06，partial）**：`laser-api` 新增 13 个测试：A-35（串口留空无默认值 → `target_required`，占位符默认值不算）、显式与 `.env` 目标解析、规范 JSON、摘要随 G-code / 发送方式 / 目标变化且不受伪造键影响、创建幂等与 JSON 不含地址、批准后只能执行一次（A-23 重复使用）、A-22（重新预览后执行 → `approval_mismatch` 并作废）、A-23（过期不可批准或执行）、到期处理、拒绝、A-32（重启后过期、同一工作流可重新批准）、失败不可重用、TTL 与入参校验。云端用 rustfmt 检查语法、用 SQLite 3.45 验证新增 SQL。**未执行**：`cargo test --workspace`（需用户本机）。
- **关联**：`crates/laser-api/src/approval.rs`、`crates/laser-api/src/lib.rs`、`crates/laser-runtime/src/db.rs`；技术书 §4.3、§5.1、§5.3–§5.5、§7.1，实施清单 P1-03，验收 A-22、A-23、A-32、A-35。

### 新增：桌面审批与审计的数据类型、数据库 schema v4（P1-01 / P1-02）

- **现象与影响**：小智或局域网发起的发送、走边框、安全动作，后续要改为“在桌面上批准后才执行”（技术书 §5.3–§5.5）。此前没有审批记录的类型和表，`audit_log` 表已建但没有任何类型和写入方法。
- **根因**：桌面审批属于融合方案新增能力，P1-00 只铺好了调用方上下文，数据层尚未建立。
- **修复**：`laser-contracts` 新增 `approval` 模块：`ApprovalStatus`（含状态迁移规则：任何状态都回不到 pending / approved，一次授权最多执行一次，失败不可重用）、`ApprovalKind`、`ApprovalSource`（由 `CallerContext` 得出）、`ApprovalConnection`（与设备连接描述同格式）、`ApprovalOutcome`、`ApprovalRecord`（`connection` 不参与序列化，接口永远拿不到连接明文）、`AuditAction` / `AuditResult` / `AuditDetail` / `AuditEntry`、`audit_client`、授权有效期夹取（默认 120 秒，30..600）与两份 JSON Schema。`laser-runtime` 迁移新增 v4：`approvals` 表（含 `connection` 列、`UNIQUE(source, request_id)`）与两个索引；新增 `RuntimeDb::schema_version`、`append_audit`、`recent_audit` 与常量 `SCHEMA_VERSION`。
- **默认与跨端**：数据库升到 v4 后旧版程序仍能打开（迁移不拒绝更高版本，新表被忽略，旧表未改）；无需降级脚本，回滚前仍建议备份 `moss.sqlite`。本步不接入任何业务入口：没有地方创建审批或写审计，现有发送、门禁与设备行为不变。`gateway_requests` 表与 `GatewayCallRequest` 按技术书 r9 随可选网关（P2b）再做。Windows / Android 一致。
- **验证补充（2026-10-06）**：用户本机 `cargo test --workspace` 全部通过。
- **验证（2026-10-06，partial）**：laser-contracts 新增 8 个单元测试（字符串与 serde 一致、状态迁移全表、记录 JSON 不含连接参数、连接参数往返与描述格式、过期判定、TTL 夹取、审计详情紧凑与往返、`audit_client` / `from_caller` 映射）；laser-runtime 新增 4 个（A-41：v4 建表且重复迁移幂等、v4 步骤失败整体回滚后重试成功、更高版本库原样打开、审计写入与类型化读回）。既有测试 `runtime_schema_seeds_twelve_builtin_cells` 中 schema 版本行数由 3 改为 `SCHEMA_VERSION`（4）——这是 schema 升级带来的必要更新，不是放宽断言。云端用 rustfmt 检查语法、用 SQLite 3.45 验证建表语句与“同名表导致失败”的行为。**未执行**：`cargo test --workspace`（需用户本机）。
- **关联**：`crates/laser-contracts/src/approval.rs`、`crates/laser-contracts/src/lib.rs`、`crates/laser-runtime/src/db.rs`、`crates/laser-runtime/src/lib.rs`；`docs/developer/db-schema.md` §3.1、§3.11、§3.12；技术书 §5.2–§5.5、§7，实施清单 P1-01、P1-02，验收 A-41。

### 重构：调用方上下文 `CallerContext` 贯穿到执行入口（P1-00，纯签名改造）

- **现象与影响**：后续的“桌面审批”与严格模式（P1-03/P1-04）需要知道一次发送、走边框、安全动作到底来自桌面、小智 MCP 桥接、局域网配对还是本机其他程序；此前这些入口拿不到调用方身份，请求体里如果夹带 `caller` 一类字段也不会被清掉。
- **根因**：HTTP 入口只把“是否回环”和来源 IP 用于鉴权，没有把调用方身份向下传；进程内 `InProcessBackend` 与本机网页走的是同一个 `handle_http`，两者无法区分。
- **修复**：`laser-contracts` 新增 `CallerContext`（`Desktop` / `Approval` / `Gateway` / `McpBridge` / `Lan` / `Loopback`，带稳定标签 `label()`）。`laser-api`：`handle_http_from` 增加可选的调用方上下文；新增 `handle_http_with_ctx`，`laser-host` 的 `InProcessBackend` 用它声明 `McpBridge`；`route_http` 在鉴权之后构造上下文（进程内声明优先，否则回环 → `Loopback`，局域网配对 → `Lan{pairing_id}`），并传给 workflow / text-task 发送、标定矩阵发送、`launch_send_job(_for)`、`handle_safe_action` 与走边框入口。上述 POST 入口在解析请求体后删除身份类字段（`caller_context`、`caller`、`_caller`、`desktop_session`、`approval_id`、`gateway_session`），身份只能由入口构造。只读的 `handle(method, path, body)` 不涉及任何执行动作，未加参数。
- **默认与跨端**：**不改任何判定**：上下文目前只传递，没有任何地方据此放行或拒绝；现有请求不含被删除的字段（已全仓检索），行为不变。Windows / Android / 浏览器一致。无设备、发送或门禁变化。
- **验证补充（2026-10-06）**：用户本机 `cargo test --workspace` 全部通过。
- **验证（2026-10-06，partial）**：新增 `caller_context_labels_are_stable`（laser-contracts）与 `caller_context_is_built_by_entry_never_from_body`（laser-api，A-36）；既有测试只为新参数补了实参（`None` / `CallerContext::Loopback`），断言未改。云端用 rustfmt 做了语法解析检查。**未执行**：`cargo test --workspace`、`cargo clippy`（云端与用户电脑工作区都没有可用的 cargo 依赖环境，需用户本机运行）。
- **关联**：`crates/laser-contracts/src/lib.rs`、`crates/laser-api/src/lib.rs`、`crates/laser-host/src/lib.rs`；技术书 §6.4，实施清单 P1-00，验收 A-36。

### 撤回：`build.windows.staticVCRuntime` 配置（本机 tauri-cli 不支持）

- **现象与影响**：为消除 `STATIC_VCRUNTIME is deprecated` 警告，曾在 `tauri.conf.json` 加入 `build.windows.staticVCRuntime: true`；用户本机 `cargo tauri build` 直接报错 `Additional properties are not allowed ('windows' was unexpected)`，无法打包。
- **根因**：用户电脑上的环境变量 `STATIC_VCRUNTIME` 为空，警告并非来自用户设置，而是本机 tauri-cli 在调用 cargo 时自行设置了该变量；该版本 CLI 的配置校验还不认识 `build.windows`，而项目依赖的较新 tauri-build 已把这个变量标为弃用——是 CLI 与 tauri-build 版本不一致，不是项目配置问题。
- **修复**：删除 `build.windows.staticVCRuntime`，`tauri.conf.json` 恢复原样。警告保留，无害：静态链接 VC 运行库仍是默认行为。日后升级 tauri-cli 到与 tauri-build 同代版本后警告会自然消失，届时无需改项目配置。
- **默认与跨端**：无行为变化。
- **验证（2026-10-06）**：用户本机报错与环境变量为空的检查结果；配置已恢复为可被当前 CLI 接受的形式（待用户重新打包确认）。
- **关联**：`apps/desktop/src-tauri/tauri.conf.json`。

### 新增：在偏好设置中选择导出目录（EXP，技术书 §9.7）

- **现象与影响**：桌面端导出（画板 PNG / 场景 JSON、G-code 下载、材料库导出）固定写入“下载”文件夹，用户无法改到自己的工作目录，也不能每次自选位置。
- **根因**：`save_export_file` 的目录写死为“下载 → 文档 → 应用数据 exports”，没有任何配置项。
- **修复**：桌面壳新增 `apps/desktop/src-tauri/src/export.rs`：命令 `get_export_settings`、`pick_export_dir`（系统文件夹选择框，`tauri-plugin-dialog`，仅桌面）、`reset_export_dir`、`set_export_ask_each_time`、`open_export_dir`、`reveal_exported_file`；`save_export_file` 改为“自定义目录 → 默认回退顺序”，开启“每次询问”时弹出“另存为”框（取消返回 `null`），新增可选参数 `fallback_default`。自定义目录在导出时校验（绝对路径、存在、可写探测），不可用时**不建目录、不改存别处**，返回 `export_dir_unavailable:<路径>|<原因>`；最终路径必须位于所选目录内。设置写入运行目录 `settings.json` 的 `export` 键，读-改-写保留其他键并以临时文件 + 重命名写入。全部命令已登记到 `build.rs` 与默认 capability。前端 `saveFile.js` 新增 `exportFile`：目录不可用时提供“本次保存到下载文件夹 / 重新选择”，取消提示“已取消导出”，成功后可“打开所在文件夹”；`appDialog` 新增三态 `showChoice`；偏好设置新增可折叠“导出”分区（仅桌面版显示）。
- **默认与跨端**：未设置时行为与之前相同（下载文件夹）；“每次询问”默认关闭。`/api/settings` 不接受导出目录（局域网页面与 HTTP 客户端不能改导出位置）。Android 与浏览器不显示该分区、行为不变。卸载永不删除导出目录。无设备、发送或门禁变化。
- **验证（2026-10-06，partial）**：Rust 新增 `export_settings_round_trip_preserves_other_keys`、`missing_or_broken_settings_file_gives_defaults`、`unavailable_custom_dir_never_falls_back_silently`、`export_target_must_stay_inside_selected_dir`（原文件名清洗与不覆盖测试迁入 `export.rs`），`frontend_invoked_commands_are_registered_and_granted` 扩充到 10 个命令；laser-api 新增 `settings_route_never_accepts_export_dir`。前端 `tests/saveFile.test.mjs` 扩充到 8 项、新增 `tests/exportSettings.test.mjs`，`npm test` 60 项通过（用户电脑工作区）。**未执行**：`cargo test --workspace`（新增依赖 `tauri-plugin-dialog`、`serde`、`serde_json`，需本机联网拉取）、`npm run build`、E2E、Windows / Android 构建与 A-82..A-88 人工测试。
- **验证补充（2026-10-06）**：用户本机构建后按 1–8 项人工测试导出目录（A-82..A-85、A-88）全部通过。
- **关联**：`apps/desktop/src-tauri/src/export.rs`、`src/lib.rs`、`Cargo.toml`、`build.rs`、`capabilities/default.json`；`crates/laser-api/src/lib.rs`（测试）；`frontend/src/saveFile.js`、`frontend/src/appDialog.js`、`frontend/src/components/AppDialog.jsx`、`frontend/src/components/ExportSettingsSection.jsx`、`frontend/src/exportSettings.js`、`frontend/src/components/SettingsModal.jsx`、`frontend/src/pages/DrawPage.jsx`、`frontend/src/pages/MaterialLabPage.jsx`、`frontend/src/styles.css`；`docs/user/quick-start.md`、`docs/developer/frontend-behavior.md`、`docs/developer/packaging-install.md`；技术书 §9.7，实施清单 EXP。

### 新增 Windows 安装包：自定义安装目录、卸载可选删除数据、运行中不强杀（PKG）

- **现象与影响**：此前只交付裸 `moss-desktop.exe`，用户无法像普通应用一样选择安装目录、从开始菜单启动或在“设置 → 应用”中卸载。
- **根因**：`tauri.conf.json` 中 `bundle.active=false`，没有安装器配置；Tauri 自带卸载器的“删除应用数据”只删 `%APPDATA%\com.moss.laser`，覆盖不到 MOSS 实际使用的 `%APPDATA%\moss`；自带的运行检查会询问后强行结束进程，会跳过急停顺序。
- **修复**：开启 NSIS 打包——`installMode: "both"`（只为我 / 所有用户，随后可自定义目录）、简体中文 + 英文、开始菜单文件夹 `MOSS`、WebView2 `embedBootstrapper`、安装器图标与曜石风格横幅 / 侧栏图。新增 `installer/hooks.nsh`：安装、卸载前若 `moss-desktop.exe`（卸载另查 `moss-host.exe`）在运行，只提示从托盘退出并取消，钩子早于模板的强杀检查执行；卸载时勾选“删除应用程序数据”（默认不勾）且不是更新模式，才删除固定路径 `%APPDATA%\moss`，`APPDATA` 为空时跳过。快速开始“安装与卸载”改为实际步骤。
- **默认与跨端**：`cargo tauri build` 现在额外生成 `target/release/bundle/nsis/MOSS_<版本>_x64-setup.exe`，首次会联网下载 NSIS 工具；`moss-desktop.exe` 照常生成。安装包未签名、`publisher` 未填写（待用户提供）。运行数据位置不变（`%APPDATA%\moss`），覆盖安装不碰数据。Android 不受影响。
- **验证（2026-10-06，partial）**：钩子插入位置（`NSIS_HOOK_PREINSTALL` 在 `CheckIfAppIsRunning` 之前）、`$DeleteAppDataCheckboxState`、`$UpdateMode`、`nsis_tauri_utils::FindProcess` 依据 Tauri 官方 `installer.nsi` / `utils.nsh` 源码核对；`tauri.conf.json` 为合法 JSON。**未执行**：本机打包、干净 Windows 虚拟机安装 / 卸载 / 升级（PK-01..PK-09）。
- **关联**：`apps/desktop/src-tauri/tauri.conf.json`、`apps/desktop/src-tauri/installer/hooks.nsh`、`apps/desktop/src-tauri/installer/header.bmp`、`apps/desktop/src-tauri/installer/sidebar.bmp`；`docs/user/quick-start.md`、`docs/developer/packaging-install.md`；技术书 §9.6，实施清单 PKG。

### 前端改版收尾：清理旧样式、视觉基线（UI-2）

- **现象与影响**：改版后 `styles.css` 仍留有已无任何组件引用的旧 HUD 样式（四角准星、监视舱准星、旧顶栏等），文件头仍是旧设计说明；改版没有可对照的外观基线。
- **根因**：第一、二批以覆盖方式迁移，没有同步删除失效规则。
- **修复**：按“类名在源码中无任何引用”删除 13 条旧规则（`.site-header`、`.hud-corner-*`、`.chamber-crosshair-*`、`.chamber-center-target`、`.hud-pill`、`.preview-image-card*`、`.job-dispatch-actions`、`.btn-job-dispatch*`），保留由模板字符串拼出的 `app-dialog--*`；更新样式文件头说明。新增 `frontend/tests/visual-baseline/`（雕刻工作台、窄屏、创作画板、材料实验室、偏好设置弹窗 5 张截图及生成方式说明）。
- **默认与跨端**：无行为变化。组件内联样式迁移为类名留作后续重构；Geist 字体待提供字体文件后自托管。
- **验证（2026-10-06）**：用户本机 `cargo test --workspace`、`npm test`、`npm run build`、`cargo tauri build` 通过，并人工验证画板 PNG / 场景导出、空画板提示、材料库导出、卡片与偏好设置折叠；删除旧规则后前端 `npm test` 54 项通过、esbuild 打包渲染无错误。**未验证（需实机，记入实施清单 §2a）**：HW-01 重复发送“已移动材料”勾选、HW-02 发送中顶栏急停、Android 真机。
- **关联**：`frontend/src/styles.css`、`frontend/tests/visual-baseline/`；实施清单 UI-02..08、§2a。

### 修复桌面端导出无反应；卡片与偏好设置改为可折叠（UI-2）

- **现象与影响**：桌面端创作画板点“场景”“PNG”导出没有任何文件；材料实验室“导出库”、创作画板预览结果里的“下载 G-code / 下载场景 JSON”在桌面端与 Android 内嵌页同样不可靠。用户另外希望各设置区可以折叠，减少滚动。
- **根因**：导出依赖网页 `<a download>`：桌面端与 Android 的 WebView 不保证处理网页下载；画板导出还在点击后立即回收 blob 地址，下载未开始就被打断；预览下载链接用 `target="_blank"` 打开新窗口，在内嵌页里没有去处。失败时只改了页面顶部一行状态字，很容易看不到。
- **修复**：新增 Tauri 命令 `save_export_file`：只接受文件名部分（替换 Windows 非法字符）、扩展名白名单 png / json / gcode / nc / txt、上限 50MB，写入“下载”文件夹（没有则退回“文档”或应用数据目录下 `exports`），同名不覆盖而是追加“ (1)”，返回实际路径；已登记到 `build.rs` 与默认 capability（并扩充 `frontend_invoked_commands_are_registered_and_granted` 测试）。前端新增 `saveFile.js`：Tauri 中调用该命令，普通浏览器里才用 `<a download>` 且延迟回收；画板 PNG / 场景、材料库导出、预览 G-code / 场景下载统一改用它，成功提示保存路径，失败弹窗说明原因。新增 `FoldHeader.jsx` 与 `foldState.js`：雕刻工作台 5 张卡、创作画板 3 张卡、材料实验室 3 张卡的标题可点击收起 / 展开；偏好设置 4 个分区同样可折叠且默认收起；收起只隐藏不卸载（已填内容、进行中的任务不受影响），状态按卡片记住。
- **默认与跨端**：导出到“下载”文件夹，不弹出系统保存对话框；Android 的“下载”目录若不可用会退回应用私有目录，路径会显示在提示中（Android 真机未验证）。折叠依赖 CSS `:has()`，旧内核不支持时卡片保持展开、不影响功能。不涉及设备连接、发送与门禁。
- **验证（2026-10-06，partial）**：新增 Rust 测试 `export_names_are_sanitized_and_restricted`、`export_paths_never_overwrite_existing_files`；前端新增 `tests/saveFile.test.mjs`（3 项）、`tests/foldState.test.mjs`（2 项），`npm test` 54 项通过（用户电脑工作区 Linux 环境）；预览桩渲染折叠后的工作台、默认收起的偏好设置，无脚本错误；`rustfmt` 解析通过。**未执行**：`cargo test`（工作区无 Rust）、`npm run build`、E2E、Windows / Android 构建与真机导出实测。
- **关联**：`apps/desktop/src-tauri/src/lib.rs`、`apps/desktop/src-tauri/build.rs`、`apps/desktop/src-tauri/capabilities/default.json`；`frontend/src/saveFile.js`、`frontend/src/foldState.js`、`frontend/src/components/FoldHeader.jsx`、`frontend/src/pages/DrawPage.jsx`、`frontend/src/pages/MaterialLabPage.jsx`、`frontend/src/pages/HomePage.jsx`、`frontend/src/components/SettingsModal.jsx`、`frontend/src/styles.css`。

### 前端改版第二批：雕刻工作台三栏、创作画板、材料库表格、弹窗（UI-2，界面变更）

- **现象与影响**：雕刻工作台把连接、内容、材质、监视舱挤在一个大卡片里的左右两栏，控制台在页面最底部；创作画板为固定高度三栏、按钮与输入仍是旧样式；材料实验室材料列表是简单列表；弹窗仍是旧配色。与定稿设计（技术书 §9.5.5 B）不一致。
- **根因**：页面版面写在组件内联样式里，第一批只替换了令牌与外壳。
- **修复**：雕刻工作台改为三栏——左栏 ①通信链路 ②加工内容 ③材质与参数，中栏 ④加工监视舱（刀路预览、统计、急停、进度），右栏加工反馈与控制台（页签改名为“终端 / 小智网关 / G 码行视图”，元素 id 不变）；去掉与顶栏重复的页面大标题；1400px 以下右栏移到下方，900px 以下单列。创作画板改为左侧大画布、右侧“材料与尺寸 / 设备 / 预览与发送”三张卡，设备地址输入框补齐样式。材料实验室材料列表改为 Aether 风格表格（加粗表头、斑马纹、悬停整行变紫、选中行左侧紫条），每行显示厚度胶囊；校准矩阵按钮行可换行，避免“发送测试”被挤出卡片。偏好设置、配对、关闭窗口、材料编辑、确认发送与应用对话框统一为曜石弹窗样式。
- **默认与跨端**：所有按钮、输入、弹窗、处理函数、接口调用与参数原样保留，只移动位置与换样式；材料表格中点击任一行的厚度胶囊直接选中该材料与厚度（调用原 `handleSelectMaterialAndThick`，以前需先选中材料才显示厚度按钮）。Windows 与 Android 共用。
- **验证（2026-10-06，partial）**：前端 `npm test` 49 项通过（用户电脑工作区 Linux 环境）；esbuild 打包后以预览桩数据在 Chromium 中渲染三页（1600 / 1280 / 1100 宽）与偏好设置弹窗，无脚本错误、无横向溢出。**未执行**：`npm run build`、`npm run test:e2e`、Windows / Android 构建，以及按功能对照清单（H1–H26、D1–D21、L1–L15、弹窗 S/A/C/O）在真机上逐项点验；Excalidraw 画布在预览中为桩组件，真实画布显示需本机确认。
- **关联**：`frontend/src/pages/HomePage.jsx`、`frontend/src/pages/DrawPage.jsx`、`frontend/src/pages/MaterialLabPage.jsx`、`frontend/src/components/ConsoleDrawer.jsx`、`frontend/src/styles.css`；技术书 §9.5.5，实施清单 UI-04..07。

### 前端改版第一批：曜石令牌、左侧导航、顶栏常驻急停（UI-2，界面变更）

- **现象与影响**：原界面为青色霓虹 HUD 风格、顶部横向标签导航，与已定稿的曜石设计（技术书 §9.5.5）不一致；顶栏没有急停，切到其他区域时只能回到监视舱找急停按钮。
- **根因**：样式散落在 `styles.css` 与约 340 处组件内联样式中，大量写死青色、霓虹绿红与藏青底色，没有统一令牌。
- **修复**：`styles.css` 顶部改为曜石令牌（近黑底、白/灰三级文字、唯一强调色激光紫、绿色只给开始加工、红色只给急停与危险、金色只给提示），旧变量名映射到新令牌；机械替换 JSX 与 CSS 中写死的旧色值；重写按钮、输入、卡片（半透明玻璃 + 光标聚光 + 渐变描边，`spotlight.js`）。新增左侧导航 `SideNav.jsx`（雕刻工作台 / 创作画板 / 材料实验室，底部偏好设置与配对认证，≤900px 折叠为顶部横排）；`Navbar.jsx` 改为顶栏：页面标题、硬件状态、小智四态与常驻“急停 0x18”。顶栏急停经 `estopBus.js` 交给当前页面，执行与该页原急停 / 取消按钮完全相同的动作（雕刻工作台 `handleEStop`，创作画板 workflow `cancel`）；当前页面没有任务时弹窗提示“请立即按控制器硬件急停”，不假装已停机。
- **默认与跨端**：不改任何接口调用、参数、门禁与存储键；原有按钮与弹窗全部保留，导航入口位置改到左侧。顶栏急停与监视舱急停一样只是软件复位请求，不能替代硬件急停。审批按钮随 P1 加入；语音中心入口随 P4 加入。Geist 字体暂未自托管（缺字体文件），先用系统字体回退，不依赖外网。Windows 与 Android 共用。
- **验证（2026-10-06，partial）**：新增 `tests/estopBus.test.mjs`（无页面接手时返回未处理、只执行第一个真正处理的页面、卸载后不再处理）；前端 `npm test` 49 项通过（用户电脑工作区 Linux 环境）；用 esbuild 打包并以预览桩数据在 Chromium 中渲染雕刻工作台、创作画板、材料实验室与 820px 窄屏，无脚本错误，顶栏急停无任务时弹出提示。**未执行**：`npm run build`、`npm run test:e2e`、Windows / Android 构建与真机查看；创作画板、材料实验室与弹窗的逐页改版（UI-04..07）尚未开始。
- **关联**：`frontend/src/styles.css`、`frontend/src/components/SideNav.jsx`、`frontend/src/components/Navbar.jsx`、`frontend/src/estopBus.js`、`frontend/src/spotlight.js`、`frontend/src/main.jsx`、`frontend/src/pages/HomePage.jsx`、`frontend/src/pages/DrawPage.jsx`（各注册顶栏急停处理）、`frontend/tests/estopBus.test.mjs`；技术书 §9.5.5，实施清单 UI-01..03。

### 顶栏“小智”状态改为真实连接状态，去掉编造的遥测（S2）

- **现象与影响**：只要填了接入点地址，顶栏就显示“小智: CONNECTED”，控制台抽屉显示客户端 `xz-mcp-broker v0.0.1`、5 条时间为 `16:00:02` 的握手日志、“已处理 3 帧”“20 项工具已就绪”——桥从未连上接入点时也会出现。用户无法据此判断小智是否真的连上，排障时会被误导。此前小智状态显示不可信。
- **根因**：`/api/system/info` 把“已配置”当成“已连接”，没有真实数据时用写死的客户端名、事件、最近请求和最小消息数填充；遥测没有记录 WebSocket 是否在线；`record_event` 把日志条数计作消息数；`tools_count` 固定 20（未开开发者开关时实际为 19）。前端控制台抽屉和偏好设置也有写死的回退值。
- **修复**：`laser-contracts` 的 `McpTelemetry` 新增 `connected`、`set_connected`、`record_message`，事件日志不再计入消息数；`laser-mcp` 桥连接建立后由 `LinkGuard` 置为已连接，以任何方式断开时置为未连接并清空客户端标识，`initialize` 未上报客户端信息时留空，`tools/list` 事件写实际工具数；`laser-api` 新增 `mcp_status_json`，只输出真实数据，新增 `mcp.connected`、`mcp.last_request_time` 与 `voice:{mode:"official", device_sessions:null}`，`tools_count` 取当前工具目录实际长度；前端新增 `state/mcpStatus.js`，顶栏胶囊按“未配置（灰）/ 未连接（红）/ 等待设备（金）/ 已连接（官方青、自建绿）”显示并标注链路，控制台抽屉与偏好设置去掉写死值（无数据显示“—”）。
- **默认与跨端**：`mcp.configured` 含义收窄为“填了接入点地址”（以前有任何活动也算）；新增字段向后兼容。“等待设备”在语音中心（P4）上线前不会出现。顶栏“审批”按钮依赖审批服务，随 P1 实施。不涉及设备连接、发送或安全门禁。Windows 与 Android 共用。
- **验证（2026-10-06，partial）**：新增 `telemetry_tests::telemetry_starts_empty_and_tracks_real_link_state`（laser-contracts）、`mcp_status_reports_only_real_telemetry` 与 `system_info_tools_count_matches_actual_catalog`（laser-api），并在 `bridge_round_trips_json_rpc_over_loopback_websocket` 中加断言：连接期间 `connected=true`、桥退出后为 `false`；前端新增 `tests/mcpStatus.test.mjs`（A-73 四态与链路文案）。前端 `npm test` 47 项通过（在用户电脑的工作区 Linux 环境执行）；改动的 4 个 JSX 文件经 esbuild 语法检查、3 个 Rust 文件经 `rustfmt` 解析通过。**未执行**：`cargo test --workspace`（该环境无 Rust 工具链、无外网）、`npm run build`、E2E、Windows / Android 构建，以及连上真实小智接入点后的状态切换实测——需用户本机补跑。
- **关联**：`crates/laser-contracts/src/lib.rs`、`crates/laser-mcp/src/lib.rs`（`LinkGuard`、`handle_json_rpc_with`、`serve_one_connection`）、`crates/laser-api/src/lib.rs`（`mcp_status_json`、`/api/system/info`）；`frontend/src/state/mcpStatus.js`、`frontend/src/components/Navbar.jsx`、`frontend/src/components/ConsoleDrawer.jsx`、`frontend/src/components/SettingsModal.jsx`、`frontend/src/main.jsx`、`frontend/tests/mcpStatus.test.mjs`；`docs/developer/api-contracts.md` §2；技术书 §9.5.3，验收 A-72 / A-73，实施清单 S2。

### 修复桌面端启动即显示“后端无法启动”：`get_backend_origin` 未通过 Tauri 权限

- **现象与影响**：Windows 桌面端打开后直接显示“MOSS 后端无法启动”；显示真实原因后可见 `Command get_backend_origin not allowed by ACL`。后端其实已经启动，是前端拿不到后端地址，整个应用不可用。
- **根因**：2026-09-26 新增 Tauri 命令 `get_backend_origin` 时，只注册了 `invoke_handler`，没有登记到 `build.rs` 的 `AppManifest` 命令清单，也没有在 `capabilities/default.json` 授权 `allow-get-backend-origin`。Tauri 2 在应用声明了命令清单后，未授权的命令一律被 ACL 拒绝；窗口导航到 `http://127.0.0.1:<端口>` 后页面仍会调用该命令，因此每次启动都失败。此前失败页只显示固定文案，掩盖了原因。与 S1 / DNS 重绑定改动无关（这些请求根本没发出）。
- **修复**：`build.rs` 命令清单加入 `get_backend_origin`；默认 capability 增加 `allow-get-backend-origin`（构建时自动生成对应权限文件）。新增测试 `frontend_invoked_commands_are_registered_and_granted`：前端会 `invoke` 的 `get_backend_origin`、`minimize_to_tray`、`exit_application` 必须同时出现在 `build.rs` 与 capability 中，防止再漏。
- **默认与跨端**：该命令只返回本机回环地址，不读写设备或文件；授权范围沿用现有 capability（主窗口、`http://127.0.0.1:*` / `http://localhost:*` 远程页）。Windows 与 Android 共用该配置。
- **验证（2026-10-06，partial）**：`rustfmt` 解析通过、capability JSON 合法。**未执行**：`cargo test --workspace`、重新构建与启动实测、Android 启动实测（云端无法拉取依赖）。需用户本机重新构建后确认主页能正常加载。
- **关联**：`apps/desktop/src-tauri/build.rs`、`apps/desktop/src-tauri/capabilities/default.json`、`apps/desktop/src-tauri/src/lib.rs`（测试）；`frontend/src/authSession.js` 的 `initializeBackendOrigin`。

### 桌面端后端启动失败时显示真实原因

- **现象与影响**：用户本机启动新构建的 `moss-desktop.exe` 后只看到“MOSS 后端无法启动 / 无法连接本机服务，请检查是否已有 MOSS 实例运行”，看不出具体原因；Windows 发行版没有控制台，`eprintln!` 打印的原因用户看不到，只能猜。
- **根因**：Tauri 宿主在 `HostApplication::init()` 或端口监听失败时，把原因替换成固定文案“后端服务初始化失败”交给前端；前端失败页也只显示固定提示。数据目录锁被占用（另一个 MOSS 或 `moss-host serve` 未退出）时，错误还是 `AlreadyLocked(..)` 这种调试格式。
- **修复**：`laser-host` 把数据目录锁占用翻译成“已有 MOSS 实例正在运行（数据目录锁 … 被占用）。请从托盘图标右键退出程序，或在任务管理器结束 moss-desktop.exe / moss-host.exe 后重试”；Tauri 宿主把初始化 / 监听失败的原因原样拼进 `get_backend_origin` 的错误；前端新增 `backendStartupErrorText`，失败页在原提示下方显示该原因（React 文本节点，不解析 HTML）。
- **默认与跨端**：只改失败时的提示，启动成功路径不变；Windows 与 Android 共用。原因里可能出现本机数据目录路径，只显示在本机窗口，不写入日志文件或网络响应。
- **验证（2026-10-06，partial）**：前端 `node --test tests/authSession.test.mjs` 4 项通过（新增失败原因文字测试）；`main.jsx` 经 esbuild 语法检查通过；Rust 改动 `rustfmt` 解析通过。**未执行**：`cargo test --workspace`、`npm run build`、Windows / Android 重新构建与启动实测（云端无法拉取依赖，用户电脑命令行挂载仍故障）。本条只让原因可见；用户截图中的实际失败原因待重新构建后读取页面确认。
- **关联**：`crates/laser-host/src/lib.rs`（`HostApplication::init`）、`apps/desktop/src-tauri/src/lib.rs`（`run`）、`frontend/src/authSession.js`、`frontend/src/main.jsx`、`frontend/tests/authSession.test.mjs`。

### 黄金快照测试不再依赖本机插着的串口

- **现象与影响**：本机插着串口设备（如 COM7）时，`cargo test --workspace` 中 `golden_api_snapshot_parity_test` 失败：`/api/ui/serial-ports` 返回 `ports:["COM7"]`、`default_port:"COM7"`，而黄金样本是空列表。测试结果随电脑上插了什么设备而变，无法作为可靠的回归门禁。
- **根因**：该路由与“串口留空自动选首个串口”的逻辑直接调用 `enumerate_system_serial_ports()` 查询真实系统，测试无法替换；黄金样本录制时机器上恰好没有串口。与本次 S1 / DNS 重绑定改动无关。
- **修复**：`ApiService` 新增可注入的串口枚举 `with_serial_port_lister`（类型 `SerialPortLister`），默认仍为真实枚举；两个调用点改为经由该函数。黄金快照测试注入空列表，继续逐字段比对原黄金样本（样本与断言均未修改、未放宽）；新增 `serial_ports_route_reports_enumerated_ports_and_first_as_default`，注入 `COM3`、`COM7` 断言列表原样返回且默认取第一个。
- **默认与跨端**：生产行为不变，Windows / Android / 浏览器均照常枚举真实串口；不涉及连接、发送或设备动作。
- **验证（2026-10-06，partial）**：用户本机已跑：`cargo test --workspace` 除本项外全部通过（laser-api 49/50）；前端 `npm test`、`npm run test:e2e`、`npm run build` 通过；Windows `cargo tauri build` 产出 `target/release/moss-desktop.exe`；Android `cargo tauri android build --target aarch64 --apk` 产出 `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk`（未签名）。本修复后需本机重跑 `cargo test --workspace` 确认全绿；云端仍无法拉取 crates 依赖，未能自行编译。
- **关联**：`crates/laser-api/src/lib.rs`（`SerialPortLister`、`with_serial_port_lister`、`connection_spec_from_request`、`/api/ui/serial-ports`、`golden_api_snapshot_parity_test`）；`tests/golden/api/serial-ports.response.json`（未改）。

### 【破坏性变更】本机连接校验 Host，防 DNS 重绑定

- **现象与影响**：S1 去掉 CORS 通配符后，仍有一条路：攻击者把自己的域名先解析到其服务器、再改解析到 127.0.0.1（DNS 重绑定），用户在本机打开该网页时，浏览器认为页面与 MOSS 同源，可以不受 CORS 限制地读取 MOSS 接口并发起写操作；而本机回环请求免鉴权。
- **根因**：S1 的“与 `Host` 同源”规则只比较 `Origin` 与 `Host` 是否一致，重绑定场景下两者都是攻击者域名，校验自然通过；后端从不检查 `Host` 本身是否是 MOSS 应有的名字。
- **修复**：`laser-api` 新增 `is_allowed_loopback_host`，`handle_http_from` 在来源校验之前执行：对端为回环地址且带 `Host` 时，主机名必须是 IPv4 / 方括号 IPv6 字面量、`localhost`（不区分大小写）或 `MOSS_ALLOWED_ORIGINS` 中出现的主机名，端口须为数字；否则任何方法（含 `GET`）返回 403 `host_not_allowed`，不进入路由、不带 CORS 头。
- **默认与跨端**：默认始终生效。桌面端与 Android 内嵌页访问 `http://127.0.0.1:<端口>`、浏览器用 `127.0.0.1` / `localhost` 打开、E2E、MCP 进程内调用、curl 均不受影响；非回环（局域网）连接不做此项，仍由 Bearer 鉴权保护，用局域网主机名访问照常；不带 `Host` 的非浏览器请求照常。**升级动作**：如果你在本机用 hosts 别名或自定义域名打开 MOSS，改用 `127.0.0.1` / `localhost`，或把 `http://<别名>:<端口>` 加入 `MOSS_ALLOWED_ORIGINS`。
- **验证（2026-10-06，partial）**：新增 `loopback_host_rules_block_dns_rebinding_names`（IP、`localhost`、大小写、IPv6、额外主机名允许；攻击者域名、`127.0.0.1.evil.example`、非法端口、未加括号 IPv6 等拒绝）与 `dns_rebinding_request_is_rejected_before_routing`（带或不带 `Origin` 的重绑定 POST 均 403 且 `settings.json` 未写入；重绑定 GET 也 403 且不泄露数据；非回环主机名访问与无 `Host` 请求照常）。既有测试 `auth_challenge_is_loopback_only_and_issued_code_pairs_remote_client` 中“本机申请配对码”的请求夹具由 `Host: remote` 改为 `Host: 127.0.0.1:9666`（本机客户端的真实写法），断言未改动。会话云端仍无法访问 crates.io，仅以替身依赖单独编译新函数与入口逻辑并通过断言；**未执行**真实依赖下的 `cargo test --workspace`、前端测试与 Windows / Android 构建（原因同 S1 条目）。
- **关联**：`crates/laser-api/src/lib.rs`（`is_allowed_loopback_host`、`host_name_without_port`、`handle_http_from`）；`docs/developer/api-contracts.md` §1.6、`docs/developer/safety.md` §2、`docs/user/quick-start.md` 常见问题；技术书 §6.5，实施清单 S1-12。

### 【破坏性变更】去掉 CORS 通配符，新增浏览器来源校验（S1）

- **现象与影响**：后端所有 JSON、文件、`OPTIONS` 响应都带 `Access-Control-Allow-Origin: *`，且回环请求免鉴权。用户在本机浏览任意网页时，该网页可以跨站读取 MOSS 接口数据；`Content-Type: text/plain` 的 POST 属于浏览器“简单请求”，不发预检就会送达，后端照样执行（例如改设置、创建或推进 workflow）。
- **根因**：`http_options`、`http_json_with_headers`、`http_bytes_with_disposition` 三个响应构造函数写死通配符，请求入口不读取 `Origin`，也没有来源校验。
- **修复**：响应构造函数不再输出任何 CORS 头；`handle_http_from` 在进入路由前按请求 `Origin` 判定（`is_allowed_origin`，精确匹配 scheme + host + port）：允许来源为 `http://tauri.localhost`、`tauri://localhost`、与请求 `Host` 同源、回环请求下的前端开发端口 5173/5177/5178（`127.0.0.1` 与 `localhost`），以及 `MOSS_ALLOWED_ORIGINS`。允许时回显来源并加 `Vary: Origin`，`Allow-Headers` 增加 `X-Moss-Desktop-Session`；不允许时 `GET`/`HEAD` 照常但不带 CORS 头，其他方法（含 `OPTIONS`）返回 403 `origin_not_allowed`，不执行业务。新增 `MOSS_ALLOWED_ORIGINS`（逗号分隔；含 `*`、缺 scheme 或带路径时宿主启动失败）。403 响应的状态行原因短语由 `HTTP` 改为 `Forbidden`。
- **默认与跨端**：默认始终生效，不依赖严格模式。不带 `Origin` 的调用（MCP 桥接、进程内调用、curl、服务端插件、局域网配对客户端、E2E 的 Node `fetch`）行为不变，只是响应不再带 CORS 头。Windows / Android 桌面端内嵌页（`http://tauri.localhost`）、MOSS 自带页面（含局域网浏览器访问）、`npm run dev` / `preview` 不受影响。**升级动作**：如果你自己在其他端口或域名托管了前端页面去调用 MOSS，需要把该来源加入 `MOSS_ALLOWED_ORIGINS`，否则其写操作会得到 403。残留风险：同源规则无法防 DNS 重绑定，留待 P1 桌面会话凭据解决（技术书 §6.5）。
- **验证（2026-10-06，partial）**：改写 `http_adapter_answers_real_loopback_request`（断言无 CORS 头、无通配符）与 `http_adapter_handles_cors_options_preflight`（断言允许来源回显、`Vary`、新 `Allow-Headers`，不允许来源 403 且无 CORS 头），未删除或放宽断言；新增 `allowed_origin_rules_are_exact`、`disallowed_origin_post_is_rejected_before_routing`（`application/json` 与 `text/plain` 两种 POST 均 403，`settings.json` 未写入、workflow 表为空；不带 `Origin` 时照常保存）、`disallowed_origin_get_is_served_without_cors_headers`、`dev_origin_is_rejected_for_non_loopback_peer`、`allowed_origin_is_echoed_on_every_response_kind`（JSON、额外来源、静态页、SSE、文件预览、`OPTIONS`）与 `laser-runtime` 的 `allowed_origins_parse_rules`。会话云端无法访问 crates.io，只能把新增函数与入口逻辑连同替身依赖单独编译并跑通断言，`rustfmt` 解析全部改动文件通过。**未执行**：真实依赖下的 `cargo test --workspace`、前端 `npm test` / `npm run test:e2e` / `npm run build`、Windows `cargo tauri build`、Android APK 构建，以及桌面端 / Android 实际 `Origin` 实测——用户电脑命令行因 Windows 更新无法挂载项目目录，需在本机补跑。
- **关联**：`crates/laser-api/src/lib.rs`（`is_allowed_origin`、`handle_http_from`、`route_http`、`with_cors_headers`、`http_options`、`ApiService::with_allowed_origins`）、`crates/laser-runtime/src/config.rs`（`parse_allowed_origins`、`allowed_origins_from_env`）、`crates/laser-host/src/lib.rs`；`docs/developer/api-contracts.md` §1.6 与 §5、`docs/developer/safety.md` §2、`docs/user/quick-start.md`；技术书 §6.5 / D11，实施清单 S1。

### 安装版读取 `%APPDATA%\moss\.env`；迁移默认目标对齐实际数据目录

- **现象与影响**：程序只从启动时的当前目录读 `.env`。将来用安装包安装、从开始菜单或桌面快捷方式启动时当前目录不确定，`.env` 里的端口、`MCP_ENDPOINT`、默认连接方式等可能全部读不到，程序悄悄回到默认值。另外 `moss-host migrate` 不写 `--target` 时把旧数据迁到“当前目录/.runtime”，而 Windows 上程序实际读写的是 `%APPDATA%\moss`，迁完后程序看不到迁移结果。
- **根因**：`LaserSettings::from_env_with_runtime_dir` 只调用 `dotenvy::dotenv()`（按当前目录查找），没有使用已经传入的运行目录；`run_migrate` 的默认目标是单独写死的，没有复用 `RuntimeRoot` 的目录规则。
- **修复**：`laser-runtime` 新增 `config::load_env_files`：先读进程环境变量，再读启动目录（向上查找）的 `.env`，最后读运行目录的 `.env`（Windows 为 `%APPDATA%\moss\.env`）；dotenvy 不覆盖已存在的变量，所以先加载的来源优先，文件不存在时静默跳过。`RuntimeRoot` 新增 `default_base()`（`APPDATA` 非空时为 `%APPDATA%\moss`，否则为当前目录 `.runtime`），`resolve(None, None)` 与 `moss-host migrate` 默认目标都改用它。
- **默认与跨端**：开发场景（仓库根目录有 `.env`）行为不变；`settings.json` 中的 `web_port`、`mcp_endpoint` 规则不变。数据目录位置不变（仍为 `%APPDATA%\moss`），无需搬数据。唯一的细微变化：`APPDATA` 被设为空字符串时，过去会得到相对路径 `moss`，现在按未设置处理，回到当前目录 `.runtime`。Android 没有 `APPDATA`，仍回落到当前目录 `.runtime`，该风险未在本次处理（见 `docs/developer/packaging-install.md`）。不涉及运动、出光、发送或固件链路。
- **验证（2026-10-05，partial）**：新增单元测试 `runtime_dir_env_file_is_loaded`、`runtime_dir_env_file_does_not_override_existing_vars`、`missing_runtime_dir_env_file_is_ignored`、`default_base_matches_platform_rule`。会话云端无法访问 crates.io，用本地替身依赖（仅模拟 dotenvy 不覆盖语义、tempfile、fs4）编译 `config.rs` 与 `root.rs`，6 项测试通过；`rustfmt --check` 对新增代码无差异。**未执行**：真实依赖下的 `cargo test --workspace`、`cargo build -p moss-host`、Windows `cargo tauri build`、Android APK 构建、从桌面快捷方式启动的实测（PK-04）——用户电脑命令行因 Windows 更新无法挂载项目目录，需在本机补跑。
- **关联**：`crates/laser-runtime/src/config.rs`、`crates/laser-runtime/src/root.rs`、`apps/moss-host/src/main.rs`；`docs/user/quick-start.md` §3、`docs/developer/packaging-install.md`、技术书 §9.6 与 PKG-2、实施清单 PKG-02。

### 桌面端背景改为 WebGL 等离子流体（界面变更，无业务行为变化）

- **现象与影响**：用户认为原背景（3D 透视网格、扫描线、550W 罗盘、粒子电弧、四角 HUD 水印）风格偏旧、观感偏素，与正在设计的新前端（技术书 §9.5.5）不一致。
- **根因**：原 `CyberBackground` 以多层 CSS 网格与 2D Canvas 叠加实现，视觉元素与新设计规范（不使用网格、扫描线、霓虹线）冲突。
- **修复**：`CyberBackground` 重写为等离子流体背景——WebGL 片元着色器（多层域扭曲噪声，紫 / 赤红 / 蓝缓慢流动，随鼠标轻微偏移）+ 2D 闪烁星点 + 暗角 + 细噪点；移除网格、扫描线、罗盘、粒子电弧、光标光斑与四角 HUD 水印及其样式。组件导出名、挂载位置与容器类名 `cyber-background-container` 不变，仍为 `aria-hidden`、`pointer-events: none`。
- **默认与跨端**：着色器以 50% 分辨率渲染后放大，限帧约 40 fps，页面隐藏时暂停；启动 2 秒内帧率低于 28 fps 自动降到 33% 分辨率并减少星点；WebGL 不可用时退回 CSS 静态渐变；`prefers-reduced-motion` 时只绘制一帧静态画面；卸载时释放 WebGL 上下文。Windows 桌面端、Android 与浏览器访问共用此组件。无接口、存储或设备链路变化。
- **验证（2026-10-05，partial）**：前端 `npm test` 44 项通过（在会话云端以原测试运行）；组件经 esbuild 打包后在 Chromium（SwiftShader WebGL）中渲染检查：正常渲染、自动降级、禁用 WebGL 回退、减少动态效果时画面静止均符合预期，无脚本错误。**未执行**：`npm run build`、`npm run test:e2e`、Windows `cargo tauri build`、Android APK 构建——本次会话云端无法访问 npm 镜像安装依赖，用户电脑上的命令行因 Windows 更新问题无法挂载项目目录；需在本机补跑并实测真实显卡下的帧率与 CPU 占用。
- **关联**：`frontend/src/components/CyberBackground.jsx`、`frontend/src/styles.css`（背景层样式段）；`docs/integration/moss-desktop-fusion-technical-spec.md` §9.5.5 C；原型 `frontend/ui-preview-forge-v2.html`。

### 发送文件一致性校验与跨端实际端口定位

- **现象与影响**：workflow、标定矩阵和断点恢复在物理 G-code SHA 门禁后会经过设备握手，再次从磁盘读取文件；这段时间若文件被改写，可能把未经确认的刀路发给设备。Android 内嵌页面在后端端口回退时仍访问固定的 9666，可能连不上或连到占用端口的其他服务。
- **根因**：发送器把第二次读取内容的哈希直接当成新的预期哈希，未与确认快照比较；Tauri 宿主知道实际端口，但移动端前端没有接收它，初始化失败还会回退到 9666。
- **修复**：把确认快照 SHA 从 workflow 和标定矩阵传入统一发送器；设备预检后再读取并比对物理文件，失败返回 `send_gate_rejected`，普通流式发送与 SD 上传只使用已比对的内存内容。Tauri 保存实际回环地址并通过 `get_backend_origin` 命令交给前端；前端取得地址后才渲染应用，初始化失败显示错误，不再猜测默认端口。Android Release 全局禁止明文，仅为 `127.0.0.1` 配置例外。架构图修正 HTTP 轮询、宿主组装及 MCP 持久化路径；移除旧 Python 约束目录，并更新 README 的软件复位文案与架构说明。
- **默认与跨端**：预览不发送，确认、安全门禁和网络/串口默认值不变；独立浏览器/Vite 开发入口仍使用 9666，桌面同源导航保留。SD 固件无法返回文件哈希时，既有流程仍明确标记 `sd_hash_verified=false`，不宣称机器端已验证。Android 本机 HTTP 例外不开放其他明文地址；已有安装需重新打包安装更新。
- **验证（2026-09-26，实机 unverified）**：新回归测试 `send_rejects_gcode_changed_after_first_sha_check_without_writing_job_lines` 在内存/持久化工作流及普通/SD 模式中，于首次校验后篡改临时文件，断言 FakeGrbl 未收到加工行或 SD 启动命令。`cargo test --workspace`、前端 `npm test`（44 项）、`npm run test:e2e`（回环 GRBL 模拟器 32 项）、`npm run build`、Windows `cargo tauri build` 与 Android `cargo tauri android build --target aarch64 --apk` 已通过。产物为 `target/release/moss-desktop.exe` 及 `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk`（未签名）；已核对 Android 合并清单与回环网络规则。Birdview rev5（14 模块、22 连接）结构校验及桌面/移动 HTML 预览通过；约束目录仅对已提交快照的 `AGENTS.md` 部分规则完成审阅，当前工作区未提交差异需另行核对。未连接硬件、未实机出光、未安装 Android 或验证物理链路。
- **关联**：`crates/laser-api/src/lib.rs` 的 `SendLauncher`、`launch_send_job_for` 与工作流/标定发送入口；`apps/desktop/src-tauri/src/lib.rs` 的 `get_backend_origin`、Android `AndroidManifest.xml` 和 `network_security_config.xml`；`frontend/src/authSession.js`、`frontend/src/main.jsx`、`frontend/tests/authSession.test.mjs`；`docs/developer/safety.md`、`.birdview/architecture.json` 与 `.birdview/constraints.reviewed.json`。

### Jev OpenRouter 反馈闭环修复

- **现象与影响**：旧 Jev 请求缺少模型且选择题字段错误，真实服务可能拒绝请求；MCP 透传分类，Web/桌面没有反馈入口，建议与实际刀路、历史无法对应。
- **根因**：服务协议、分类转参数、生成器实际参数和 workflow 持久化未串联；原生成器只取材料推荐值。
- **修复**：仅接入 OpenRouter Decisions，固定 `typesafe/jev-1.13` 和 `criteria` 契约；API 共用分类与有界调参，低置信度/矛盾反馈不调整，失败明确回退本地规则。新预览保存实际参数；反馈原文、判断、建议和已采用参数分别原子写入 workflow 历史。界面及 MCP 采用建议后生成独立新预览，实际应用功率/速度/次数并冻结新 SHA，绝不自动发送；旧任务参数不全不猜测。
- **默认与升级**：Jev 默认关闭；请求超时 5 秒、置信度门槛 0.8（尚未实机校准）。只支持 OpenRouter Key，旧 TypeSafe Key 需替换；取消可编辑服务地址。API Key 存储方式保持不变。Web、桌面、Android 共用前端和后端。旧 `/api/jev/feedback` 保留别名，但现在必须关联 workflow；这是反馈 API 的契约变更。
- **验证（2026-09-22）**：`cargo test --workspace` 171 项通过；前端 `npm test` 42 项、`npm run test:e2e` 回环 GRBL 模拟器 32 项通过；反馈浏览器 mock 在 1280px/390px 通过提交、回退、采用、刷新历史与无发送/无溢出检查，并审阅截图。严格 HTTP mock 校验模型/题型/criteria/Bearer；覆盖异常响应、低置信度、参数边界、SQLite 采用事务回滚、状态写回保留历史、三类素材实际参数/尺寸/SHA。`npm run build`、Windows `cargo tauri build`、Android `cargo tauri android build --target aarch64 --apk` 均最终成功。产物为 `target/release/moss-desktop.exe`、`apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk`（未签名、未安装）；保留既有格式，不执行全仓格式化。真实 OpenRouter 凭据请求、实机安装及物理激光链路 unverified。Birdview rev4 结构与当前任务约束渲染通过；本地 HTML 浏览器策略阻止交互预览，图的视觉验收 unverified。
- **关联**：`crates/laser-api/src/feedback.rs`、`RuntimeStore::save_feedback`、MCP `dispatch_tool_call_with`、前端 `FeedbackPanel`。

- **[Raster] 图片黑白约定与规划器统一，避免预览和实际加工区域相反**：本次实际刀路断言发现 draw/image 入口将黑色记为 1，而底层 `raster_plan_with_direction` 以 0 为出光区域，可能雕刻背景或令全黑素材没有加工段。两处生成入口统一为黑 0、白 255，画板路径预览同步该约定；测试通过全黑 PNG 检查实际 S/F 指令、轮次、尺寸与 SHA。旧已生成文件不改写，需要重新预览；物理效果未实机验证。

### 🚀 新功能 (Features)

- **[Jev/Settings] 增加可选 Jev 加工反馈判断服务**：桌面设置页可保存启用状态、服务地址和 API Key；普通配置进入 `settings.json`，API Key 进入 SQLite `app_secrets`，默认只显示 `sk-xxxx` 式遮罩，点击眼睛才请求完整 Key。MCP 的反馈调参工具优先调用 Jev，网络失败、未配置或未启用时回退到原有本地规则；Jev 只返回判断/建议，不接触设备发送链路。验证覆盖 SQLite 密钥迁移与读写、API Key 脱敏/查看/删除、Jev mock 入口和前端构建；真实 Jev 服务与硬件链路未验证。

- **[Desktop/小智] 新增设置页 MCP 网关端点配置**：桌面 EXE 可直接输入并保存小智 `ws://`/`wss://` 接入点，配置写入运行目录 `settings.json`，启动时优先读取并兼容回退环境变量 `MCP_ENDPOINT`；界面只显示脱敏端点，留空保存可关闭桥接，保存后需重启生效。

- **[FluidNC/Integration] 深度集成 FluidNC 现代控制与网络架构（$CD 配置提取、SD 脱机加工、红光走边框 Framing 与自适应标尺）**：
  - **YAML 硬件配置自动感知**：新增 `parse_fluidnc_yaml` 与 `DeviceConnection::fetch_fluidnc_config`，在设备握手探测时自动发送 `$CD` 并解析机床 X/Y 最大行程（`max_travel_mm`）、激光功率曲线（`speed_map`）以及气吹（`air_assist`）与安全门（`door`）外设引脚；当设备为传统 GRBL 时自动回退至从 `$$` 的 `$130/$131` 提取尺寸，`/api/connection/check` 与 `/api/device/probe` 自动透传 `machine_limits` 与 `features`，前端画板可动态约束加工有效范围杜绝撞机。
  - **SD 卡 / LittleFS 脱机离线加工**：在 `laser-device` 中新增 `wait_until_sd_completed`，持续追踪状态报告中的 `|SD:xx.xx%,/sd/job.gcode|` 真实执行进度；在 `laser-api` 与前端确认弹窗中增加 `offline_sd` 脱机模式支持，解算完成后直接发起 `$SD/Run` 本地脱机加工，彻底规避长达数小时的任务因电脑休眠、手机切后台或 Wi-Fi 抖动导致工件报废。
  - **红光轮廓预览走边框 (Framing)**：在 `laser-core/src/gcode.rs` 中新增 `generate_framing_gcode` 与 `generate_paths_framing_gcode`，根据图元外接矩形自动生成微弱光斑（或纯空走）循环走边框刀路；后端新增 `/api/device/framing` 接口并在前端 `DrawPage.jsx` 侧边栏增加【走边框 (Framing)】专属按钮，加工前一键直观校准材料摆放位置。
  - **功率标尺动态自适应**：引入 `verify_job_ready_adaptive` 与 `rescale_gcode_line_s`，遇到默认 `$30=255` 的 FluidNC 固件时不再强行拒绝，在保持原文件 SHA-256 门禁安全的前提下于底层通信流水线中自动进行 S 值等比例缩放（如 1000->255、500->128）。

- **[Frontend/CNC] 雕刻下发全链路升级为数控机床级中控台（JobWorker 逐行下发 + 发光实时进度条 + 三标签遥测抽屉）**：
  - **顶栏双指示胶囊解耦**：将原本单一模糊的 `OFFLINE` 拆分为独立的【⚡ 硬件: OFFLINE/ONLINE】（专指 GRBL 激光硬件控制器）与【🤖 小智: CONNECTED/OFFLINE】（专指小智 AI 语音/MCP 网关），彻底消除“右上角离线但底部在线”的认知冲突。
  - **串口助手级真实端口枚举**：移除串口下拉菜单中硬编码的“自动检测串口”与“COM1~COM6预设”，对齐串口助手仅列出系统真实识别到的可用串口列表，未检测到时明确提示；支持启动自动静默探测与下发前自动兜底握手。
  - **监视舱视野优化**：彻底删除加工预览中央多余的十字准星与中心靶心线条，保留纯净的雷达网格与激光工件轨迹。
  - **核心操作卡片**：主页右侧监控舱下方内嵌醒目的大号【0x18 软复位物理急停 (E-STOP)】红色制动按钮（独占整行，带高危警示光晕），以及展示刀路总行数、物理 SHA-256 指纹与预估耗时的【刀路摘要胶囊】；全界面统一保留左侧主控区唯一个【启动 JobWorker 逐行下发】按钮。
  - **发光实时进度条 (STREAMING PROGRESS)**：激光青色发光渐变光槽与呼吸光晕，实时透视 `lines_sent / total_lines` 行数与百分比。
  - **三标签遥测控制台抽屉 (ConsoleDrawer)**：
    - `>_ 通信终端与状态监控 (CONSOLE FEED)`：等宽黑客风黑底终端，支持 `[DISPATCH]`、`[STREAM]`、`[SUCCESS]`、`[GRBL]`、`[ESTOP]` 色彩标签高亮与自动吸底；预览生成时规范输出 `[PREVIEW]` 标签及解算刀路行数与 SHA-256 状态；
    - `小智 MCP 桥接状态`：通过 `/api/system/info` 与 `McpTelemetry` 全局单例实时捕获小智客户端身份（`xz-mcp-broker v0.0.1`）、握手协议（`2024-11-05`）与最新请求（`tools/list`），并在下方内嵌【小智 JSON-RPC 实时交互流 (RPC AUDIT LOG)】终端视窗，真实逐帧打印小智请求流水，拒绝任何静态数据伪装；
    - `G-CODE 实时行视图`：代码高亮亮度升级（亮白高对比度文字 `#f1f5f9`、金属蓝行号 `#818cf8` 与高能青色执行行高亮），下发时根据 `lines_sent` 自动平滑滚动并将当前执行行高亮变青色（带 `►` 激光光标），并在右侧提供【清空视图】按钮便于快速清理重置大文件代码。
  - **跨端与响应式适配**：对移动端竖屏（360~430px）提供自适应横向 Tab 滑动与百分比满宽，桌面端（`moss-desktop.exe`）与 Android 端（`app-arm64-debug.apk`）已全量重建。
- **[McpBridge] 小智 WebSocket 桥接落地**：`moss-host serve` 与桌面端启动时读取 `MCP_ENDPOINT`（`wss://…?token=…`），以 tungstenite + rustls(ring, webpki 根证书) 同步客户端连接小智控制台，逐帧处理 `initialize / ping / tools/list / tools/call` JSON-RPC，断线按 1s→60s 指数退避重连，日志中 token 脱敏；未配置或仍为模板占位符时静默不启动 (`crates/laser-mcp/src/lib.rs` `run_bridge`、`crates/laser-host/src/lib.rs` `spawn_mcp_bridge`)。
- **[McpTools] 工具调用接入 moss-host 真实后端**：`laser_workflow_tool`、`run_calibration_grid_tool`、`select_calibration_cell_tool`、`material_params_tool`、`generate_text_laser_task_tool`、`start_tuned_job_tool`、`regenerate_text_laser_task_tool` 通过 `HostBackend` 转发到进程内 `ApiService`，与网页/桌面端共享同一套 SHA 门禁、SQLite 存储与 JobWorker；`refine_laser_params_from_feedback_tool` 实现有界调参规则（太焦/太浅/切不透/毛边/断线）。
- **[MaterialLab] 标定矩阵全链路真实化**：`/api/material-lab/preview` 调 `laser_core::material::calibration_matrix` 按行列/功率/速度区间生成单元格与 G-code 并落盘、冻结 SHA、写入 `calibrations` 记录；`/api/material-lab/send` 复用 workflow 同款门禁与 JobWorker 真实逐行下发；`/api/material-lab/select-cell` 把选中单元格功率/速度/次数写回材料库（文件 + SQLite）；新增 `/api/material-lab/status` 供轮询。前端渲染真实单元格参数并轮询发送进度。
- **[TypeBoundary] 类型边界守卫改为 Rust 集成测试** `crates/laser-contracts/tests/type_boundary.rs`，随 `cargo test --workspace` 执行，CI 不再依赖 Python。

### 🔄 变更 (Changes)

- **[Docs/Architecture] 全面重构文档体系与契约对齐**：
  - **结构规范化与分层归档**：将 `docs/` 根目录下分散的技术文档、测试报告与图片进行系统性分层，重组为用户（`docs/user/`）、开发者（`docs/developer/`）、激光硬件（`docs/laser/`）、小智集成（`docs/integration/`）与历史归档（`docs/archive/`）五大专属目录，全仓 Markdown 相对链接已全部通过自动化校验，杜绝任何断链。
  - **活文档重写与规范固化**：新增 `docs/developer/architecture.md`（真实反映 Rust 进程模型、crate 拓扑、同步 TCP 宿主与自适应流控）与 `docs/developer/frontend-behavior.md`（固化前端状态词表、重发防呆、覆层映射与本地存储键）；重写 `docs/user/quick-start.md`（整合原 Windows 指南，覆盖双模启动、配对与脱机操作）、`docs/developer/standards.md`、`docs/developer/safety.md`、`docs/laser/README.md` 与 `docs/laser/xiaozhi-grbl-tool-boundary.md`；更新 `api-contracts.md`（收录全部 28 个路由并修正字段错误）与 `db-schema.md`（对齐 SQLite schema v2 并标注未接线数据表）；更新小智提示词 `latest.md` 与历史快照。
  - **历史留痕与废弃清理**：新建 `docs/archive/` 并创建归档总览 `README.md`，将重构阶段性产物（`rust-rebuild-spec.md`、`MIGRATION_STATUS.md`、`ACCEPTANCE_REPORT.md`、`APP_BUTTONS_TEST_REPORT.md`、`frontend-inventory.md`、`voice-image-gcode-engine.md`）移入归档并统一追加只读归档横幅，移除硬编码的本地私有路径；彻底删除已废弃的旧版 Python `windows.md`、空重定向存根以及两份一次性展示 HTML；将 3 张非文档测试位图移入 `tests/fixtures/images/`。
- **[Frontend/Serial] 扩充串口波特率预设梯队**：在 `frontend/src/pages/HomePage.jsx` 中将原本单一的 3 个选项扩充为覆盖主流与高速场景的完整标准梯队（`9600`、`19200`、`38400`、`57600`、`115200 (默认)`、`230400`、`250000`、`460800`、`921600`），无缝适配 ESP32、FluidNC 极速流式传输与各类微控制器调试，保持 115200 为默认选中项。
- 整理公开仓库目录，区分应用、文档、脚本和资源。
- 补充贡献指南、安全策略和 CI 配置。
- **[Repo] 彻底移除 Python 运行时遗留**：删除 `pyproject.toml`、`venv/`、`migration/`（黄金样本生成与差分脚本）、`tests/test_*.py`（28 个用例文件均 import 已删除的 `core/tools/moss_mcp`，仅靠本机 venv 指向旁路目录才能运行）；`AGENTS.md`、`CONTRIBUTING.md`、`CLAUDE.md`、`docs/developer/*` 的验证命令统一为 `cargo test --workspace` + `npm test` + `npm run test:e2e`。黄金样本 `tests/golden/` 与字体夹具保留。
- **[McpTools] 兜底分支不再伪装成功**：未实现或需要后端的工具在无后端时返回 `success:false` 并说明原因；`laser_grbl_tool/laser_network_grbl_tool` 的 `send_file/send_command` 明确拒绝并引导走安全 workflow。
- **[Capabilities] `/v1/capabilities`** 的 `serial` 改为 true（串口传输已实现），`command_execution` 反映 `MOSS_ENABLE_DEVELOPER_COMMANDS` 开关。
- **[laser-mcp] 移除未使用的 `rmcp` 依赖**（仅存在一个编译探针），改为 `tungstenite 0.26` + `rustls 0.23(ring)`；依赖树不含 OpenSSL / aws-lc / tokio。

### 🐛 修复 (Bug Fixes)

#### [Safety/Material] 预览、发送和断点恢复现在统一执行材料安全门禁
- **现象与影响**：`laser_core::safety::evaluate_material` 只在核心示例中使用，实际 preview/send 路由可以继续生成或尝试发送 PVC 等危险材料；`scripts/start.bat -Server` 还会固定打开已过时的 `127.0.0.1:8766`，端口回退时无法访问实际服务。
- **根因分析**：API 的文字、图片、workflow 和材料矩阵路径缺少统一的材料安全检查；启动脚本在宿主输出实际监听地址前就使用了历史固定端口。
- **修复措施**：`laser-api` 在文字/图片/G-code workflow 预览、workflow 确认发送与断点恢复、材料矩阵预览与发送前调用 `evaluate_material`，PVC/明确危险材料返回 `material_safety_blocked`，成分不明塑料要求澄清；`start.bat` 改为解析 `moss-host` 的实际监听地址后再打开浏览器。
- **验证**：`cargo test --workspace` 通过（含 laser-core 37 项、laser-api 35 项及设备/宿主/工具回归）；前端 `npm test` 42 项与 `npm run build` 通过；Windows Tauri Debug 构建通过。Android APK 构建仍被既有 Android 条件编译错误阻塞，未连接硬件、未发送 G-code。
- **关联代码与文档**：`crates/laser-api/src/lib.rs`、`scripts/start.bat`、`docs/developer/{safety,architecture,api-contracts}.md`、`docs/integration/xiaozhi-prompts/latest.md`。

#### [LaserApi/Build] 修复材料管理删除分支导致桌面端 Release 构建失败
- **现象与影响**：执行 Tauri 桌面端 Release 构建时，`laser-api` 在 `handle_material_manage` 的删除分支编译失败，因而无法生成桌面 `.exe`。
- **根因分析**：该函数返回 `Vec<u8>`，但材料安全校验使用了只能用于 `Result`/`Option` 返回函数的 `?` 运算符。
- **修复措施**：将校验错误转换为现有的 HTTP `400` JSON 失败响应，并保留 `material_safety_blocked` 错误码。
- **验证**：重新执行前端构建和 Tauri Release 构建；最终结果以本次构建命令输出及产物检查为准。
- **关联代码**：`crates/laser-api/src/lib.rs`、`handle_material_manage`。

#### [Testing/Contracts] 修复测试套件中的伪成功断言与 API 快照覆盖缺口
- **问题**：部分 E2E 用例使用本地变量、自造 ID、条件跳过和宽松终态判断；Rust 测试存在恒真断言、实际执行 shell 的测试替身、只校验 HTTP 状态码的 golden 测试；项目根目录缺少 workspace 清单，导致约定的 `cargo test --workspace` 无法执行。
- **修复**：移除伪造回退值并强制校验真实上传文件、工作流 ID、取消可恢复状态和 GRBL 通信证据；将命令执行测试改为注入 runner；补充根 workspace；golden 测试改为深度比较响应内容，并保持材料库导出协议对默认 `power_scale` 的兼容。
- **验证**：补充 E2E 临时目录清理和关键通信事件检查；通过 `cargo metadata`、相关 Rust crate 测试、前端单测和 `moss-host --release` 构建验证。

#### [Frontend/E2E-Tests] 彻底清理 E2E 测试套件中虚假凑数的自测自断言代码，剔除伪造闭环并排除测试临时脏目录
- **现象与影响 (Symptom & Impact)**：
  - 在此前 `frontend/tests/e2e/buttons.test.mjs` 中，宣称覆盖“37 项全功能 UI 按钮与状态闭环”，但在 `[BTN-11]`（日志清空）、`[BTN-21]`（路由返回）、`[BTN-22/23]`（场景与图元导出）、`[BTN-25]`（材质切换）、`[BTN-27]`（画板配置保存）、`[BTN-29]`（防呆模拟）、`[BTN-32]`（Tab 切换）、`[BTN-34]`（关闭策略）、`[BTN-36]`（Token 清除）、`[BTN-37]`（退出急停）等用例中，直接在测试闭包内部通过 `let` 局部变量自赋值、`new Map()` 原生集合存取或硬编码布尔值来强行满足 `assert.equal`；
  - 该行为并未引入或调用任何真实被测业务模块与 DOM 事件，虚构了 100% 通过的“伪绿灯”战报，严重掩盖了前端状态机与安全门禁的真实可用性，违背了测试真实性底线；同时运行 E2E 测试时在工作区残留了 `.tmp-test-e2e` 脏数据。
- **根因分析 (Root Cause Analysis)**：
  - 测试编写者为了在进度指标上达成“全量 UI 按钮自动化覆盖”，在尚未接入真实无头浏览器（Headless Browser / Playwright）自动化渲染与交互驱动的情况下，将原本需要驱动真实 UI 渲染的测试用例退化为测试文件内部的纯 JavaScript 变量赋值和原生 API 调用，形成了“自写变量、自调赋值、自言成功”的形式主义闭环。
- **修复措施 (Fix & Implementation)**：
  - ① 坚决果断彻底剔除 `frontend/tests/e2e/buttons.test.mjs` 中的 11 处虚假自测用例（`BTN-11, 21, 22, 23, 25, 27, 29(虚假防呆段), 32, 34, 36, 37`），剥离虚构的 UI 按钮包装；
  - ② 将该测试套件正名为“核心 API 与硬件通信工作流端到端真实测试”，仅保留经过真实 TCP 网络通信、回环 GRBL 仿真器握手、`$$` 固件校验、JobWorker 逐行 ACK、设备只读探测与后端 API 真实交互的 26 项高置信度硬核端到端用例；
  - ③ 彻底删除工作区下散落的 `.tmp-test-e2e` 与 `.tmp-test` 临时目录，并在根目录创建标准 `.gitignore` 规则确保测试脏产物绝不入库；
  - ④ 确立“先核对真实需求再写测试”的原则，真正的前端 UI 逻辑测试严格通过专有的前端单元测试（如已有的 40 项单测）或后续经核对授权的真实端到端驱动实现。
- **验证证据与回归测试 (Verification & Regression)**：
  - 执行 `cd frontend && npm test`，确认已有的 40 项前端核心单元测试（覆盖 `repeatSendGuard`、`appDialog`、`authSession`、`materialSelectState`、`statusNormalize` 等）全量通过且均引入了真实源文件；
  - 确认 `frontend/tests/e2e/buttons.test.mjs` 与 `scripts/test_app_buttons.mjs` 中虚假自测用例完全清零，测试命名如实反映实际覆盖范围。
- **关联代码与索引 (References)**：
  - `frontend/tests/e2e/buttons.test.mjs`；
  - `scripts/test_app_buttons.mjs:184-188`；
  - `.gitignore`；
  - `memory/honest-meaningful-testing.md`。

#### [LaserDevice/FluidNC] 字符计数流控升级为通道感知与动态自适应，消除 127 字节瓶颈与雕刻微停顿焦斑
- **现象与影响**：在雕刻复杂矢量曲线或高密度微小线段图案时，ESP32/FluidNC 激光雕刻机在拐角处仍偶发微停顿，在加工材料上留下焦黑凹斑，且整体加工速度受限于上位机单步发包节奏。
- **根因分析**：`JobWorker` 固定采用了旧版 8 位 AVR 单片机的 `DEFAULT_RX_BUFFER_SIZE = 127` 字节配额；而现代 FluidNC 运行在 ESP32 上，串口/网络接收缓冲实际为 512~1024 字节，规划器缓冲多达 35 块；固定 127 字节在面对微线段时无法充分填满下位机规划器，导致规划器饥饿（Planner Starvation）。
- **修复措施**：① 传输层抽象增加 `default_rx_buffer_size()`，网络连接（`TcpGrbl`）初始默认窗口提升至 1024 字节，物理串口（`SerialGrbl`）初始保持 255 字节安全向下兼容；② 在 `JobWorker::stream_lines_with_buffer_size` 的响应消费循环中，动态解析状态报告中的 `|Bf:blocks,rx_bytes`，将流控窗口自适应扩容至硬件真实报告的空闲容量（`clamp(255, 1024)`），大幅提升预推密度并消除微停顿。
- **关联代码**：涉及 `crates/laser-device/src/lib.rs:110, 195, 230, 1140-1230`，配套单测 `stream_lines_dynamically_adapts_to_reported_rx_buffer`。

#### [LaserDevice/Diagnostics] 报错与报警诊断全量扩充覆盖 FluidNC 专有 Alarm 11~18 与 Error 码体系
- **现象与影响**：当机床发生启动引脚冲突、未回零锁定或 SD 卡未插入等故障时，上位机控制台与小智语音仅能输出抽象数字代码或未知错误（如 `ALARM:11`、`error:60`、`error:150`），用户无法获知真实的物理故障原因，排查成本高。
- **根因分析**：旧实现中 `diagnose_device_message` 仅内置了老版 GRBL 基础报警（1~10 号）与 7 个常见 G-code 语法错误码，缺少 FluidNC 现代体系针对外设引脚、脱机文件系统和安全策略的专有扩展映射。
- **修复措施**：在 `diagnose_device_message` 中全量扩充对 FluidNC 专有代码的自然语言翻译：① 报警体系新增 11（启动引脚触发）、12（回零歧义）、13（硬停止）、14（强制回零锁定）、15（配置加载锁定）、16（IO扩展复位）、17（几何冲突）、18（探针撞限位）；② 错误体系新增 40（启动引脚异常）、60（SD卡挂载失败/未插）、61/66（文件打不开）、65（文件不存在）、67（文件系统忙）、80/81（数值无效）、110（Web认证失败）、120（接口被WebUI占用）、130（点动取消）、150/152（引脚配置或YAML语法错误）。
- **关联代码**：涉及 `crates/laser-device/src/lib.rs:1290-1360`，配套单测 `diagnose_device_message_translates_fluidnc_alarms_and_errors`。

#### [LaserApi/OfflineSD] SD 卡脱机作业实现任务唯一指纹命名、HTTP 上传与硬件 SHA-256 物理对账闭环
- **现象与影响**：脱机雕刻任务原先仅向机床发送 `$SD/Run=/sd/job.gcode` 指令，未在下发前执行文件上传与校验；若 SD 卡中无此文件或存在上一任加工的旧文件，机床直接报 `error:65` 或误雕刻上一次的历史任务，造成材料报废与出光安全风险。
- **根因分析**：缺少面向机床存储介质的传输与对账闭环；使用固定的文件名称极易引发同名文件覆写残留与状态混淆。
- **修复措施**：① 引入任务与哈希唯一命名规则 `/sd/moss_{safe_wf_id}_{short_sha}.gcode` 杜绝命名冲突与残留覆盖；② 支持通过机床 Web/HTTP API 自动将解算好的物理 G-code 上传至 SD 卡；③ 上传后自动发送 `$File/ShowHash` 请求机床固件自计算并核验 SHA-256 物理指纹，哈希不匹配即刻阻断；④ 握手校验一致后发送 `$SD/Run`，并通过 `wait_until_sd_completed` 实时捕获固件状态报告中的 `|SD:xx.xx%|` 真实加工进度直至完成。
- **关联代码**：`crates/laser-api/src/lib.rs:600-665`、`crates/laser-device/src/lib.rs:1320-1420`。
